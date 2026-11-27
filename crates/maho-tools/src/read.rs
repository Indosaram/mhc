//! Port of `packages/coding-agent/src/core/tools/read.ts`.

pub use crate::read_core::{
    ReadExecutionPolicy, ReadFolder, ReadOperations, ReadSummaryInput, ReadToolDetails,
    ReadToolInput, detect_supported_image_mime_type,
};

use crate::definition::{ToolContent, ToolDefinition, ToolError, ToolFuture};
use crate::filesystem_policy::{FilesystemPolicyChecker, canonicalize_filesystem_path};
use crate::machine::{ToolMachine, default_tool_machine};
use crate::read_core::{
    CoreReadToolOptions, create_core_read_tool_definition, machine_read_path_with,
};
use base64::Engine;
use maho_agent::harness::utils::{
    read_folders::{prepare::prepare_read_folder, selected_read_folder},
    segmented_read_view::create_default_read_summary,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct LocalReadOperations;

impl ReadOperations for LocalReadOperations {
    fn read_file<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, Vec<u8>> {
        Box::pin(async move {
            reject_dataless(path).await?;
            Ok(tokio::fs::read(path)
                .await
                .map_err(|e| crate::definition::node_fs_error(e, "open", path))?)
        })
    }
    fn access<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, ()> {
        Box::pin(async move {
            reject_dataless(path).await?;
            tokio::fs::File::open(path)
                .await
                .map_err(|e| crate::definition::node_fs_error(e, "access", path))?;
            Ok(())
        })
    }
    fn detect_image_mime_type<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, Option<String>> {
        Box::pin(async move {
            use tokio::io::AsyncReadExt;
            reject_dataless(path).await?;
            let mut file = tokio::fs::File::open(path)
                .await
                .map_err(|e| crate::definition::node_fs_error(e, "open", path))?;
            let mut bytes = vec![0; 4100];
            let count = file.read(&mut bytes).await?;
            bytes.truncate(count);
            Ok(detect_supported_image_mime_type(&bytes).map(str::to_owned))
        })
    }
    fn read_checked<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, (Option<String>, Vec<u8>)> {
        Box::pin(async move {
            reject_dataless(path).await?;
            let buffer = tokio::fs::read(path)
                .await
                .map_err(|e| crate::definition::node_fs_error(e, "open", path))?;
            let mime = detect_supported_image_mime_type(&buffer[..buffer.len().min(4100)])
                .map(str::to_owned);
            Ok((mime, buffer))
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
        .map_err(|error| crate::definition::node_fs_error(error, "access", path))?;
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

/// The measured folder senpi passes as `folder: selectedReadFolder`.
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
    pub context_model: Option<bool>,
    pub operations: Option<Arc<dyn ReadOperations>>,
    pub filesystem_policy: Option<FilesystemPolicyChecker>,
    pub execution_policy: Option<ReadExecutionPolicy>,
    pub machine: Option<ToolMachine>,
}

impl Default for ReadToolOptions {
    fn default() -> Self {
        Self {
            folder: Some(Arc::new(SelectedReadFolder)),
            auto_resize_images: None,
            resize_options: None,
            context_model: None,
            operations: None,
            filesystem_policy: None,
            execution_policy: None,
            machine: None,
        }
    }
}

pub fn process_image(
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
        let has_alpha = decoded.color().has_alpha();
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
                let fits = |len: usize| len.div_ceil(3) * 4 < original_size_limit;
                let png_len = candidates.first().map(|(data, _)| data.len());
                let png_fits = png_len.is_some_and(&fits);
                let jpeg_index = candidates
                    .iter()
                    .skip(1)
                    .position(|(data, _)| fits(data.len()))
                    .map(|index| index + 1);
                let jpeg_len = jpeg_index
                    .and_then(|index| candidates.get(index))
                    .map(|(data, _)| data.len());
                let take_png = match jpeg_len {
                    None => png_fits,
                    Some(jpeg_len) => {
                        png_fits && (has_alpha || png_len.is_some_and(|png| png <= jpeg_len))
                    }
                };
                let chosen = match (take_png, jpeg_index) {
                    (true, _) => candidates.into_iter().next(),
                    (false, Some(index)) => candidates.into_iter().nth(index),
                    (false, None) => None,
                };
                if let Some((data, kind)) = chosen {
                    bytes = data;
                    mime = kind.into();
                    hints.push(format!(
                        "[Image: original {width}x{height}, displayed at {w}x{h}. Multiply coordinates by {:.2} to map to original image.]",
                        f64::from(width) / f64::from(w)
                    ));
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
    let supplied_machine = options.machine.is_some();
    let machine = options.machine.unwrap_or_else(default_tool_machine);
    let canonical_machine = machine.clone();
    let execution_policy = Some(options.execution_policy.unwrap_or_default());

    let core_opts = CoreReadToolOptions {
        resolve_read_path: if supplied_machine {
            Some(machine_read_path_with(
                machine.clone(),
                crate::path_utils::resolve_to_cwd,
            ))
        } else {
            Some(Arc::new(|path, cwd| {
                Box::pin(async move {
                    Ok(crate::path_utils::resolve_read_path_async(&path, &cwd).await)
                })
            }))
        },
        machine,
        folder: options.folder,
        auto_resize_images: options.auto_resize_images,
        resize_options: options.resize_options,
        context_model: options.context_model,
        operations: options.operations,
        filesystem_policy: options.filesystem_policy,
        execution_policy,
        canonicalize_path: if supplied_machine {
            Some(Arc::new(move |path| {
                let machine = canonical_machine.clone();
                Box::pin(async move {
                    let context = machine.context(None, None);
                    let ops = machine.ops();
                    ops.files()
                        .canonicalize(&path, &context)
                        .await
                        .map_err(crate::fs_error_code::tool_error_from_machine)
                })
            }))
        } else {
            Some(Arc::new(|path| {
                Box::pin(async move { canonicalize_filesystem_path(&path).await })
            }))
        },
        detect_image_mime_type: if supplied_machine {
            None
        } else {
            Some(Arc::new(|path| {
                Box::pin(async move { LocalReadOperations.detect_image_mime_type(&path).await })
            }))
        },
        process_image: Some(Arc::new(process_image)),
    };

    let mut tool = create_core_read_tool_definition(cwd, core_opts);
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
        let bytes = b"unchanged".to_vec();
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
        let image = image::DynamicImage::new_rgb8(1, 1);
        let mut encoded = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut encoded, image::ImageFormat::Png)
            .expect("encode fixture");
        let bytes = encoded.into_inner();
        let limit = bytes.len() + 1;
        assert!(bytes.len().div_ceil(3) * 4 >= limit);
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

#[cfg(test)]
mod read_input_tests {
    use super::*;
    fn parse(extra: serde_json::Value) -> Result<ReadToolInput, serde_json::Error> {
        let mut params = serde_json::json!({"path": "a"});
        if let (Some(target), Some(source)) = (params.as_object_mut(), extra.as_object()) {
            target.extend(source.clone());
        }
        serde_json::from_value(params)
    }
    #[test]
    fn offset_accepts_integral_numbers_and_rejects_others() {
        assert_eq!(
            parse(serde_json::json!({"offset": 100.0}))
                .expect("100.0")
                .offset,
            Some(100)
        );
        assert_eq!(
            parse(serde_json::json!({"offset": 7})).expect("7").offset,
            Some(7)
        );
        assert!(parse(serde_json::json!({"offset": 1.5})).is_err());
        assert!(parse(serde_json::json!({"offset": -1})).is_err());
    }
    #[test]
    fn limit_accepts_integral_numbers_and_rejects_others() {
        assert_eq!(
            parse(serde_json::json!({"limit": 100.0}))
                .expect("100.0")
                .limit,
            Some(100)
        );
        assert_eq!(
            parse(serde_json::json!({"limit": 7})).expect("7").limit,
            Some(7)
        );
        assert!(parse(serde_json::json!({"limit": 1.5})).is_err());
        assert!(parse(serde_json::json!({"limit": -1})).is_err());
    }
}
