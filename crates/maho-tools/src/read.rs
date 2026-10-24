use crate::{definition::*, filesystem_policy::*, model_only_text::model_only_text, truncate::*};
use base64::Engine;
use maho_agent::harness::utils::{
    read_folders::{prepare::prepare_read_folder, selected_read_folder},
    segmented_read_view::create_default_read_summary,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
pub trait ReadOperations: Send + Sync {
    fn read_file<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, Vec<u8>>;
    fn access<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, ()>;
    fn detect_image_mime_type<'a>(&'a self, _path: &'a Path) -> ToolFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}
pub struct LocalReadOperations;
impl ReadOperations for LocalReadOperations {
    fn read_file<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, Vec<u8>> {
        Box::pin(async move {
            reject_dataless(path).await?;
            Ok(tokio::fs::read(path)
                .await
                .map_err(|e| node_fs_error(e, "open", path))?)
        })
    }
    fn access<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, ()> {
        Box::pin(async move {
            reject_dataless(path).await?;
            tokio::fs::File::open(path)
                .await
                .map_err(|e| node_fs_error(e, "access", path))?;
            Ok(())
        })
    }
    fn detect_image_mime_type<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, Option<String>> {
        Box::pin(async move {
            use tokio::io::AsyncReadExt;
            reject_dataless(path).await?;
            let mut file = tokio::fs::File::open(path)
                .await
                .map_err(|e| node_fs_error(e, "open", path))?;
            let mut bytes = vec![0; 4100];
            let count = file.read(&mut bytes).await?;
            bytes.truncate(count);
            Ok(detect_supported_image_mime_type(&bytes).map(str::to_owned))
        })
    }
}
const SF_DATALESS: u32 = 0x4000_0000;
pub fn flags_are_dataless(flags: u32) -> bool {
    flags & SF_DATALESS != 0
}
#[cfg(target_os = "macos")]
async fn reject_dataless(path: &Path) -> Result<(), ToolError> {
    use std::os::macos::fs::MetadataExt;
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|error| node_fs_error(error, "access", path))?;
    if flags_are_dataless(metadata.st_flags()) {
        return Err(ToolError::Message(
            "File contents are not downloaded from iCloud; download the file before reading it."
                .into(),
        ));
    }
    Ok(())
}
#[cfg(not(target_os = "macos"))]
async fn reject_dataless(_path: &Path) -> Result<(), ToolError> {
    Ok(())
}

#[derive(Clone)]
pub struct ReadExecutionPolicy {
    deadline: Duration,
    capacity: Arc<tokio::sync::Semaphore>,
}
impl ReadExecutionPolicy {
    pub fn new(deadline: Duration, maximum_concurrent: usize) -> Self {
        Self {
            deadline,
            capacity: Arc::new(tokio::sync::Semaphore::new(maximum_concurrent)),
        }
    }
    pub fn with_deadline(&self, deadline: Duration) -> Self {
        Self {
            deadline,
            capacity: Arc::clone(&self.capacity),
        }
    }
    async fn run<T: Send + 'static>(
        &self,
        signal: &AbortSignal,
        operation: impl std::future::Future<Output = Result<T, ToolError>> + Send + 'static,
    ) -> Result<T, ToolError> {
        let capacity = Arc::clone(&self.capacity);
        let mut task = tokio::spawn(async move {
            let _permit = capacity
                .acquire_owned()
                .await
                .map_err(|error| ToolError::Message(error.to_string()))?;
            operation.await
        });
        tokio::select! {
            result = &mut task => result.map_err(|error| ToolError::Message(error.to_string()))?,
            () = signal.cancelled() => Err(ToolError::Aborted),
            () = tokio::time::sleep(self.deadline) => Err(ToolError::Message("File read timed out".into())),
        }
    }
}
impl Default for ReadExecutionPolicy {
    fn default() -> Self {
        Self::new(Duration::from_secs(60), 8)
    }
}
pub struct ReadSummaryInput<'a> {
    pub path: &'a Path,
    pub text: &'a str,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
    pub truncated: bool,
}
pub trait ReadFolder: Send + Sync {
    fn summarize<'a>(&'a self, input: ReadSummaryInput<'a>) -> ToolFuture<'a, Option<String>>;
}
/// The measured folder senpi passes as `folder: selectedReadFolder`: prepares the selected
/// harness folder for the path and renders the default segmented read summary.
pub struct SelectedReadFolder;
impl ReadFolder for SelectedReadFolder {
    fn summarize<'a>(&'a self, input: ReadSummaryInput<'a>) -> ToolFuture<'a, Option<String>> {
        Box::pin(async move {
            let path = input.path.to_string_lossy();
            let selected = selected_read_folder();
            let folder = prepare_read_folder(&path, Some(selected.clone()), &selected);
            Ok(create_default_read_summary(
                &path,
                input.text,
                input.offset,
                input.limit,
                folder.as_deref(),
                input.truncated,
            )
            .map(|rendered| rendered.text))
        })
    }
}
#[derive(Clone)]
pub struct ReadToolOptions {
    pub folder: Option<Arc<dyn ReadFolder>>,
    pub auto_resize_images: Option<bool>,
    pub resize_options: Option<maho_ai::types::ModelImageResizeOptions>,
    pub operations: Option<Arc<dyn ReadOperations>>,
    pub filesystem_policy: Option<FilesystemPolicyChecker>,
    pub execution_policy: Option<ReadExecutionPolicy>,
}
impl Default for ReadToolOptions {
    fn default() -> Self {
        Self {
            folder: Some(Arc::new(SelectedReadFolder)),
            auto_resize_images: None,
            resize_options: None,
            operations: None,
            filesystem_policy: None,
            execution_policy: None,
        }
    }
}
#[derive(Deserialize)]
pub struct ReadToolInput {
    pub path: String,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}
pub fn detect_supported_image_mime_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[255, 216, 255]) {
        return if bytes.get(3) == Some(&247) {
            None
        } else {
            Some("image/jpeg")
        };
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        if bytes.get(8..16) != Some(&b"\0\0\0\rIHDR"[..]) {
            return None;
        }
        let mut offset = 8;
        while offset + 8 <= bytes.len() {
            let length = u32::from_be_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ]) as usize;
            if bytes.get(offset + 4..offset + 8) == Some(&b"acTL"[..]) {
                return None;
            }
            if bytes.get(offset + 4..offset + 8) == Some(&b"IDAT"[..]) {
                break;
            }
            offset = offset.saturating_add(length).saturating_add(12);
        }
        return Some("image/png");
    }
    if bytes.starts_with(b"GIF") {
        return Some("image/gif");
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(&b"WEBP"[..]) {
        return Some("image/webp");
    }
    if bytes.starts_with(b"BM") && bytes.len() >= 26 {
        let uint = |i| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
        let size = uint(2);
        let pixel = uint(10);
        let dib = uint(14);
        if (size != 0 && size < 26)
            || pixel < 14u32.saturating_add(dib)
            || (size != 0 && pixel >= size)
        {
            return None;
        }
        let offset = if dib == 12 {
            22
        } else if (40..=124).contains(&dib) && bytes.len() >= 30 {
            26
        } else {
            return None;
        };
        let planes = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
        let bits = u16::from_le_bytes([bytes[offset + 2], bytes[offset + 3]]);
        if planes == 1 && [1, 4, 8, 16, 24, 32].contains(&bits) {
            return Some("image/bmp");
        }
    }
    None
}
fn process_image(
    bytes: Vec<u8>,
    mime: String,
    resize: bool,
    resize_options: Option<maho_ai::types::ModelImageResizeOptions>,
) -> Result<(Vec<ToolContent>, String), ToolError> {
    let mut bytes = bytes;
    let mut mime = mime
        .split(';')
        .next()
        .unwrap_or(&mime)
        .trim()
        .to_lowercase();
    let mut hints = Vec::new();
    if ![
        "image/png",
        "image/jpeg",
        "image/jpg",
        "image/gif",
        "image/webp",
    ]
    .contains(&mime.as_str())
    {
        let converted = image::load_from_memory(&bytes).and_then(|image| {
            let mut out = std::io::Cursor::new(Vec::new());
            image.write_to(&mut out, image::ImageFormat::Png)?;
            Ok(out.into_inner())
        });
        match converted {
            Ok(data) => {
                hints.push(format!("[Image converted from {mime} to image/png.]"));
                bytes = data;
                mime = "image/png".into();
            }
            Err(_) => {
                return Ok((
                    Vec::new(),
                    "[Image omitted: could not be converted to a supported inline image format.]"
                        .into(),
                ));
            }
        }
    }
    if mime == "image/jpg" {
        mime = "image/jpeg".into();
    }
    if resize {
        let decoded = (|| -> image::ImageResult<image::DynamicImage> {
            use image::ImageDecoder;
            let mut decoder = image::ImageReader::new(std::io::Cursor::new(&bytes))
                .with_guessed_format()?
                .into_decoder()?;
            let orientation = decoder.orientation()?;
            let mut decoded = image::DynamicImage::from_decoder(decoder)?;
            decoded.apply_orientation(orientation);
            Ok(decoded)
        })();
        let decoded = match decoded {
            Ok(image) => image,
            Err(_) => {
                return Ok((
                    Vec::new(),
                    "[Image omitted: could not be resized below the inline image size limit.]"
                        .into(),
                ));
            }
        };
        let (width, height) = (decoded.width(), decoded.height());
        let max_width = resize_options
            .as_ref()
            .and_then(|options| options.max_width)
            .unwrap_or(2000);
        let max_height = resize_options
            .as_ref()
            .and_then(|options| options.max_height)
            .unwrap_or(2000);
        let original_size_limit = resize_options
            .as_ref()
            .and_then(|options| options.max_bytes)
            .unwrap_or(4_718_592);
        let jpeg_quality = resize_options
            .as_ref()
            .and_then(|options| options.jpeg_quality)
            .unwrap_or(80);
        let mut qualities = Vec::new();
        for quality in [jpeg_quality, 85, 70, 55, 40] {
            if !qualities.contains(&quality) {
                qualities.push(quality);
            }
        }
        if width > max_width
            || height > max_height
            || bytes.len().div_ceil(3) * 4 >= original_size_limit
        {
            let (mut w, mut h) = (width, height);
            if w > max_width {
                h = ((u64::from(h) * u64::from(max_width) + u64::from(w) / 2) / u64::from(w))
                    as u32;
                w = max_width;
            }
            if h > max_height {
                w = ((u64::from(w) * u64::from(max_height) + u64::from(h) / 2) / u64::from(h))
                    as u32;
                h = max_height;
            }
            loop {
                let resized =
                    decoded.resize_exact(w.max(1), h.max(1), image::imageops::FilterType::Lanczos3);
                let mut out = std::io::Cursor::new(Vec::new());
                resized
                    .write_to(&mut out, image::ImageFormat::Png)
                    .map_err(|e| ToolError::Message(e.to_string()))?;
                let mut candidates = vec![(out.into_inner(), "image/png")];
                for &quality in &qualities {
                    let mut data = Vec::new();
                    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut data, quality)
                        .encode_image(&resized.to_rgb8())
                        .map_err(|e| ToolError::Message(e.to_string()))?;
                    candidates.push((data, "image/jpeg"));
                }
                if let Some((data, kind)) = candidates
                    .into_iter()
                    .find(|(data, _)| data.len().div_ceil(3) * 4 < original_size_limit)
                {
                    bytes = data;
                    mime = kind.into();
                    hints.push(format!("[Image: original {width}x{height}, displayed at {w}x{h}. Multiply coordinates by {:.2} to map to original image.]", f64::from(width)/f64::from(w)));
                    break;
                }
                if w == 1 && h == 1 {
                    return Ok((
                        Vec::new(),
                        "[Image omitted: could not be resized below the inline image size limit.]"
                            .into(),
                    ));
                }
                w = (w * 3 / 4).max(1);
                h = (h * 3 / 4).max(1);
            }
        }
    }
    let mut note = format!("Read image file [{mime}]");
    if !hints.is_empty() {
        note.push('\n');
        note.push_str(&hints.join("\n"));
    }
    Ok((
        vec![ToolContent::Image {
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            mime_type: mime,
        }],
        note,
    ))
}
pub fn create_read_tool_definition(cwd: PathBuf, options: ReadToolOptions) -> ToolDefinition {
    let server_pinned = options.operations.is_some() || options.filesystem_policy.is_some();
    let ops = options
        .operations
        .clone()
        .unwrap_or_else(|| Arc::new(LocalReadOperations));
    let execution_policy = options.execution_policy.clone().unwrap_or_default();
    let execute: ToolExecutor = Arc::new(move |call| {
        let cwd = cwd.clone();
        let options = options.clone();
        let ops = Arc::clone(&ops);
        let execution_policy = execution_policy.clone();
        Box::pin(async move {
            let input: ReadToolInput = serde_json::from_value(call.params)?;
            if input.path.to_ascii_lowercase().starts_with("local://") {
                return Err(ToolError::Message("local:// URIs resolve only inside eval cells via the kernel read()/write() helpers; the read tool takes filesystem paths. Re-read this with the eval read() helper, or retry with the plain absolute file path.".into()));
            }
            call.signal.check()?;
            let work = async {
                let path = crate::path_utils::resolve_read_path_async(
                    &input.path,
                    call.context.map_or(cwd.as_path(), ToolContext::cwd),
                )
                .await;
                check_filesystem_policy(
                    options.filesystem_policy.as_ref(),
                    &path,
                    FilesystemOperation::Read,
                    "read",
                )
                .await?;
                let (mime, buffer) = execution_policy
                    .run(&call.signal, {
                        let ops = Arc::clone(&ops);
                        let path = path.clone();
                        async move {
                            ops.access(&path).await?;
                            let mime = ops.detect_image_mime_type(&path).await?;
                            let buffer = ops.read_file(&path).await?;
                            Ok((mime, buffer))
                        }
                    })
                    .await?;
                if let Some(mime) = mime {
                    let original_mime = mime.clone();
                    let resize = options.auto_resize_images.unwrap_or(true);
                    let resize_options = call
                        .context
                        .and_then(ToolContext::model)
                        .and_then(|model| model.input_limits.as_ref())
                        .and_then(|limits| limits.images.as_ref())
                        .and_then(|images| images.resize.as_ref())
                        .or(options.resize_options.as_ref())
                        .cloned();
                    let (mut images, mut note) = tokio::task::spawn_blocking(move || {
                        process_image(buffer, mime, resize, resize_options)
                    })
                    .await
                    .map_err(|e| ToolError::Message(e.to_string()))??;
                    if images.is_empty() {
                        note = format!("Read image file [{original_mime}]\n{note}");
                    }
                    if call
                        .context
                        .and_then(ToolContext::model)
                        .is_some_and(|m| !m.input.contains(&maho_ai::types::InputModality::Image))
                    {
                        note.push_str("\n[Current model does not support images. The image will be omitted from this request.]");
                    }
                    let mut content = vec![ToolContent::text(note)];
                    content.append(&mut images);
                    return Ok(ToolResult {
                        content,
                        details: None,
                        structured_content: None,
                        is_error: None,
                    });
                }
                let text = String::from_utf8_lossy(&buffer);
                let lines: Vec<_> = text.split('\n').collect();
                let start = input.offset.unwrap_or(1).saturating_sub(1);
                if start >= lines.len() {
                    return Err(ToolError::Message(format!(
                        "Offset {} is beyond end of file ({} lines total)",
                        input.offset.unwrap_or(1),
                        lines.len()
                    )));
                }
                let end = input.limit.map_or(lines.len(), |limit| {
                    start.saturating_add(limit).min(lines.len())
                });
                let truncation =
                    truncate_head(&lines[start..end].join("\n"), TruncationOptions::default());
                let summary = if let Some(folder) = &options.folder {
                    folder
                        .summarize(ReadSummaryInput {
                            path: &path,
                            text: &text,
                            offset: input.offset,
                            limit: input.limit,
                            truncated: truncation.truncated,
                        })
                        .await?
                } else {
                    None
                };
                let mut details = None;
                let mut notice = None;
                let output = if let Some(summary) = summary {
                    summary
                } else if truncation.first_line_exceeds_limit {
                    notice = Some(format!(
                        "[Line {} is {}, exceeds {} limit. Use bash: sed -n '{}p' {} | head -c {}]",
                        start + 1,
                        format_size(lines[start].len()),
                        format_size(DEFAULT_MAX_BYTES),
                        start + 1,
                        input.path,
                        DEFAULT_MAX_BYTES
                    ));
                    details = Some(json!({"truncation":truncation}));
                    String::new()
                } else if truncation.truncated {
                    let display_end = start + truncation.output_lines;
                    notice = Some(format!(
                        "[Showing lines {}-{display_end} of {}{}. Use offset={} to continue.]",
                        start + 1,
                        lines.len(),
                        if truncation.truncated_by.as_deref() == Some("lines") {
                            String::new()
                        } else {
                            format!(" ({} limit)", format_size(DEFAULT_MAX_BYTES))
                        },
                        display_end + 1
                    ));
                    details = Some(json!({"truncation":truncation}));
                    truncation.content
                } else {
                    if input.limit.is_some() && end < lines.len() {
                        notice = Some(format!(
                            "[{} more lines in file. Use offset={} to continue.]",
                            lines.len() - end,
                            end + 1
                        ));
                    }
                    truncation.content
                };
                let mut content = Vec::new();
                if !output.is_empty() || notice.is_none() {
                    content.push(ToolContent::text(format!(
                        "{output}{}",
                        if notice.is_some() { "\n" } else { "" }
                    )));
                }
                if let Some(notice) = notice {
                    content.push(model_only_text(notice));
                }
                Ok(ToolResult {
                    content,
                    details,
                    structured_content: None,
                    is_error: None,
                })
            };
            tokio::select! { result = work => result, () = call.signal.cancelled() => Err(ToolError::Aborted) }
        })
    });
    let mut tool = ToolDefinition::new(
        "read",
        "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to 2000 lines or 50KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.",
        json!({"type":"object","properties":{"path":{"type":"string","description":"Path to the file to read (relative or absolute)"},"offset":{"type":"number","description":"Line number to start reading from (1-indexed)"},"limit":{"type":"number","description":"Maximum number of lines to read"}},"required":["path"]}),
        execute,
    );
    tool.constrained_sampling = Some(maho_ai::types::ConstrainedSampling::Config(
        maho_ai::types::ConstrainedSamplingConfig::JsonSchema {
            strict: maho_ai::types::JsonSchemaStrictness::Prefer,
        },
    ));
    tool.prompt_snippet = Some("Read file contents".into());
    tool.prompt_guidelines = Some(vec![
        "Use read to examine files instead of cat or sed.".into(),
    ]);
    if server_pinned {
        tool.server_pinned = Some(true);
    }
    tool
}

#[cfg(test)]
mod image_resize_tests {
    use super::*;
    #[test]
    fn absent_dimensions_resolve_independently() {
        // Given a landscape image and profiles specifying only one dimension.
        let image = image::DynamicImage::new_rgb8(120, 60);
        let mut encoded = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut encoded, image::ImageFormat::Png)
            .expect("encode fixture");
        for (max_width, max_height, expected) in [
            (Some(40), None, (40, 20)),
            (None, Some(20), (40, 20)),
            (None, None, (120, 60)),
        ] {
            // When the partial model profile reaches the read consumer.
            let (content, _) = process_image(
                encoded.get_ref().clone(),
                "image/png".into(),
                true,
                Some(maho_ai::types::ModelImageResizeOptions {
                    max_width,
                    max_height,
                    ..Default::default()
                }),
            )
            .expect("process fixture");
            // Then only the specified axis overrides the resize defaults.
            let ToolContent::Image { data, .. } = &content[0] else {
                panic!("image attachment")
            };
            let decoded = image::load_from_memory(
                &base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .expect("base64"),
            )
            .expect("image");
            assert_eq!((decoded.width(), decoded.height()), expected);
        }
    }

    #[test]
    fn custom_jpeg_quality_is_the_first_jpeg_candidate() {
        // Given deterministic pixels whose PNG exceeds the chosen JPEG budget.
        let pixels = image::RgbImage::from_fn(64, 64, |x, y| {
            image::Rgb([
                u8::try_from((x * 37 + y * 19) % 256).expect("byte"),
                u8::try_from((x * 11 + y * 53) % 256).expect("byte"),
                u8::try_from((x * 71 + y * 7) % 256).expect("byte"),
            ])
        });
        let fixture = image::DynamicImage::ImageRgb8(pixels);
        let resized = fixture.resize_exact(32, 32, image::imageops::FilterType::Lanczos3);
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 12)
            .encode_image(&resized.to_rgb8())
            .expect("jpeg fixture");
        let limit = jpeg.len().div_ceil(3) * 4 + 1;
        let mut png = std::io::Cursor::new(Vec::new());
        resized
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("png candidate");
        assert!(png.get_ref().len().div_ceil(3) * 4 >= limit);
        let mut input = std::io::Cursor::new(Vec::new());
        fixture
            .write_to(&mut input, image::ImageFormat::Png)
            .expect("input");
        // When resizing requires JPEG with a non-default quality.
        let (content, _) = process_image(
            input.into_inner(),
            "image/png".into(),
            true,
            Some(maho_ai::types::ModelImageResizeOptions {
                max_width: Some(32),
                max_height: None,
                max_bytes: Some(limit),
                jpeg_quality: Some(12),
            }),
        )
        .expect("process image");
        // Then the first accepted JPEG uses the requested quality, not 80.
        let ToolContent::Image { data, mime_type } = &content[0] else {
            panic!("image attachment")
        };
        assert_eq!(mime_type, "image/jpeg");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .expect("base64"),
            jpeg
        );
        assert!(data.len() < limit);
    }

    #[test]
    fn disabled_resize_preserves_bytes_despite_model_byte_limit() {
        // Given resizing disabled and an impossible model resize ceiling.
        let bytes = b"unchanged".to_vec();
        // When processing a supported MIME type.
        let (content, _) = process_image(
            bytes.clone(),
            "image/png".into(),
            false,
            Some(maho_ai::types::ModelImageResizeOptions {
                max_bytes: Some(1),
                ..Default::default()
            }),
        )
        .expect("process image");
        // Then resize limits do not override the explicit opt-out.
        let ToolContent::Image { data, .. } = &content[0] else {
            panic!("image attachment")
        };
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .expect("base64"),
            bytes
        );
    }

    #[test]
    fn custom_limit_applies_to_base64_payload() {
        // Given an image whose encoded payload is larger than its raw bytes.
        let image = image::DynamicImage::new_rgb8(1, 1);
        let mut encoded = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut encoded, image::ImageFormat::Png)
            .expect("encode fixture");
        let bytes = encoded.into_inner();
        let limit = bytes.len() + 1;
        assert!(bytes.len().div_ceil(3) * 4 >= limit);
        // When the custom ceiling cannot fit even the smallest image.
        let (content, _) = process_image(
            bytes,
            "image/png".into(),
            true,
            Some(maho_ai::types::ModelImageResizeOptions {
                max_width: Some(1),
                max_height: Some(1),
                max_bytes: Some(limit),
                jpeg_quality: None,
            }),
        )
        .expect("process image");
        // Then the image is omitted rather than admitted using a raw-byte ceiling.
        assert!(content.is_empty());
    }
    #[test]
    fn model_resize_profile_controls_output_dimensions() {
        let image = image::DynamicImage::new_rgb8(120, 60);
        let mut encoded = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut encoded, image::ImageFormat::Png)
            .expect("encode fixture");
        let (content, _) = process_image(
            encoded.into_inner(),
            "image/png".into(),
            true,
            Some(maho_ai::types::ModelImageResizeOptions {
                max_width: Some(40),
                max_height: Some(40),
                max_bytes: None,
                jpeg_quality: None,
            }),
        )
        .expect("resize image");
        let ToolContent::Image { data, .. } = content.first().expect("resized image attachment")
        else {
            panic!("resized image attachment expected");
        };
        let decoded = image::load_from_memory(
            &base64::engine::general_purpose::STANDARD
                .decode(data)
                .expect("decode base64"),
        )
        .expect("decode image");
        assert_eq!((decoded.width(), decoded.height()), (40, 20));
    }
}
