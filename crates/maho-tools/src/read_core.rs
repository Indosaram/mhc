//! Port of `packages/coding-agent/src/core/tools/read-core.ts`.

pub use crate::read_text::{ReadFolder, ReadSummaryInput, ReadToolDetails, shape_text_read};

use crate::definition::{
    AbortSignal, ToolContent, ToolContext, ToolDefinition, ToolError, ToolExecutor, ToolFuture,
    ToolResult, ToolWorkspaceEffect,
};
use crate::encoding::encode_base64;
use crate::file_tool_host::{
    ToolPathCanonicalizer, lexical_canonical_path, resolve_posix_tool_path,
    resolve_read_path_variants,
};
use crate::filesystem_policy::{
    FilesystemOperation, FilesystemPolicyChecker, check_filesystem_policy,
};
use crate::fs_error_code::tool_error_from_machine;
use crate::tool_machine::ToolMachine;
use maho_machine_ops::{OpName, ReadRequest, StatAccess, StatRequest};
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const IMAGE_TYPE_SNIFF_BYTES: usize = 4100;

pub const READ_PROMPT_SNIPPET: &str = "Read file contents";
pub const READ_PROMPT_GUIDELINE: &str = "Use read to examine files instead of cat or sed.";
pub const READ_PROMPT_SHORT_GUIDELINE: &str = "Files: read, not cat/sed.";

#[derive(Deserialize)]
pub struct ReadToolInput {
    pub path: String,
    #[serde(default, deserialize_with = "crate::json_usize::option_usize")]
    pub offset: Option<usize>,
    #[serde(default, deserialize_with = "crate::json_usize::option_usize")]
    pub limit: Option<usize>,
}

pub trait ReadOperations: Send + Sync {
    fn read_file<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, Vec<u8>>;
    fn access<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, ()>;
    fn detect_image_mime_type<'a>(&'a self, _path: &'a Path) -> ToolFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
    /// One open per read: access, sniff and payload in a single pass.
    fn read_checked<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, (Option<String>, Vec<u8>)> {
        Box::pin(async move {
            self.access(path).await?;
            let mime = self.detect_image_mime_type(path).await?;
            let buffer = self.read_file(path).await?;
            Ok((mime, buffer))
        })
    }
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

    pub async fn run<T: Send + 'static>(
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

pub type AsyncReadPathResolver =
    Arc<dyn Fn(String, PathBuf) -> ToolFuture<'static, PathBuf> + Send + Sync>;

pub type ImageMimeDetector =
    Arc<dyn Fn(PathBuf) -> ToolFuture<'static, Option<String>> + Send + Sync>;

pub type ImageProcessorFn = Arc<
    dyn Fn(
            Vec<u8>,
            String,
            bool,
            Option<maho_ai::types::ModelImageResizeOptions>,
        ) -> Result<(Vec<ToolContent>, String), ToolError>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct CoreReadToolOptions {
    pub machine: ToolMachine,
    pub folder: Option<Arc<dyn ReadFolder>>,
    pub auto_resize_images: Option<bool>,
    pub resize_options: Option<maho_ai::types::ModelImageResizeOptions>,
    pub context_model: Option<bool>,
    pub operations: Option<Arc<dyn ReadOperations>>,
    pub filesystem_policy: Option<FilesystemPolicyChecker>,
    pub execution_policy: Option<ReadExecutionPolicy>,
    pub resolve_read_path: Option<AsyncReadPathResolver>,
    pub canonicalize_path: Option<ToolPathCanonicalizer>,
    pub detect_image_mime_type: Option<ImageMimeDetector>,
    pub process_image: Option<ImageProcessorFn>,
}

impl CoreReadToolOptions {
    pub fn new(machine: ToolMachine) -> Self {
        Self {
            machine,
            folder: None,
            auto_resize_images: None,
            resize_options: None,
            context_model: None,
            operations: None,
            filesystem_policy: None,
            execution_policy: None,
            resolve_read_path: None,
            canonicalize_path: None,
            detect_image_mime_type: None,
            process_image: None,
        }
    }
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

fn inline_image_as_is(
    bytes: Vec<u8>,
    mime: String,
    _resize: bool,
    _resize_options: Option<maho_ai::types::ModelImageResizeOptions>,
) -> Result<(Vec<ToolContent>, String), ToolError> {
    let lower = mime.to_lowercase();
    if matches!(
        lower.as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) {
        let note = format!("Read image file [{lower}]");
        return Ok((
            vec![ToolContent::Image {
                data: encode_base64(&bytes),
                mime_type: lower,
            }],
            note,
        ));
    }
    Ok((
        Vec::new(),
        "[Image omitted: could not be converted to a supported inline image format.]".to_owned(),
    ))
}

fn machine_read_path(machine: ToolMachine) -> AsyncReadPathResolver {
    machine_read_path_with(machine, resolve_posix_tool_path)
}

pub(crate) fn machine_read_path_with(
    machine: ToolMachine,
    resolve: fn(&str, &Path) -> PathBuf,
) -> AsyncReadPathResolver {
    Arc::new(move |file_path, cwd| {
        let m = machine.clone();
        Box::pin(async move {
            let resolved =
                resolve_read_path_variants(&file_path, &cwd, &resolve, |candidate: PathBuf| {
                    let m = m.clone();
                    async move {
                        let ctx = m.context(None, None);
                        m.ops()
                            .files()
                            .stat(
                                StatRequest {
                                    path: candidate.to_string_lossy().into_owned(),
                                    access: None,
                                },
                                &ctx,
                            )
                            .await
                            .is_ok()
                    }
                })
                .await;
            Ok(resolved)
        })
    })
}

struct MachineReadOperations {
    machine: ToolMachine,
    signal: AbortSignal,
    detect_image_mime_type: Option<ImageMimeDetector>,
}

impl ReadOperations for MachineReadOperations {
    fn read_file<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, Vec<u8>> {
        Box::pin(async move {
            let ctx = self.machine.context(Some(&self.signal), None);
            let res = self
                .machine
                .ops()
                .files()
                .read(
                    ReadRequest {
                        path: path.to_string_lossy().into_owned(),
                        offset: None,
                        limit_bytes: None,
                    },
                    &ctx,
                )
                .await
                .map_err(tool_error_from_machine)?;
            Ok(res.bytes)
        })
    }

    fn access<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, ()> {
        Box::pin(async move {
            let ctx = self.machine.context(Some(&self.signal), None);
            self.machine
                .ops()
                .files()
                .stat(
                    StatRequest {
                        path: path.to_string_lossy().into_owned(),
                        access: Some(StatAccess::Read),
                    },
                    &ctx,
                )
                .await
                .map(|_| ())
                .map_err(tool_error_from_machine)
        })
    }

    fn detect_image_mime_type<'a>(&'a self, path: &'a Path) -> ToolFuture<'a, Option<String>> {
        Box::pin(async move {
            if let Some(detector) = &self.detect_image_mime_type {
                return detector(path.to_path_buf()).await;
            }
            let ctx = self.machine.context(Some(&self.signal), None);
            let head = self
                .machine
                .ops()
                .files()
                .read(
                    ReadRequest {
                        path: path.to_string_lossy().into_owned(),
                        offset: Some(0),
                        limit_bytes: Some(IMAGE_TYPE_SNIFF_BYTES),
                    },
                    &ctx,
                )
                .await
                .map_err(tool_error_from_machine)?;
            Ok(detect_supported_image_mime_type(&head.bytes).map(str::to_owned))
        })
    }
}

pub fn create_core_read_tool_definition(
    cwd: PathBuf,
    options: CoreReadToolOptions,
) -> ToolDefinition {
    let auto_resize_images = options.auto_resize_images.unwrap_or(true);
    let fallback_resize_options = options.resize_options;
    let context_model = options.context_model.unwrap_or(true);
    let custom_ops = options.operations;
    let machine = options.machine;
    let folder = options.folder;
    let filesystem_policy = options.filesystem_policy;
    let execution_policy = options.execution_policy;
    let resolve_read_path = options
        .resolve_read_path
        .unwrap_or_else(|| machine_read_path(machine.clone()));
    let canonicalize_path = options
        .canonicalize_path
        .unwrap_or_else(lexical_canonical_path);
    let detect_image_mime_type = options.detect_image_mime_type;
    let process_image: ImageProcessorFn = options
        .process_image
        .unwrap_or_else(|| Arc::new(inline_image_as_is));

    let execute: ToolExecutor = Arc::new(move |call| {
        let cwd = cwd.clone();
        let custom_ops = custom_ops.clone();
        let machine = machine.clone();
        let folder = folder.clone();
        let filesystem_policy = filesystem_policy.clone();
        let execution_policy = execution_policy.clone();
        let resolve_read_path = Arc::clone(&resolve_read_path);
        let canonicalize_path = Arc::clone(&canonicalize_path);
        let detect_image_mime_type = detect_image_mime_type.clone();
        let fallback_resize_options = fallback_resize_options.clone();
        let process_image = Arc::clone(&process_image);

        Box::pin(async move {
            let input: ReadToolInput = serde_json::from_value(call.params)?;
            if input.path.to_ascii_lowercase().starts_with("local://") {
                return Err(ToolError::Message(
                    "local:// URIs resolve only inside eval cells via the kernel read()/write() helpers; the read tool takes filesystem paths. Re-read this with the eval read() helper, or retry with the plain absolute file path."
                        .into(),
                ));
            }
            call.signal.check()?;
            let work = async {
                let gate_args =
                    json!({ "path": input.path, "offset": input.offset, "limit": input.limit });
                if let Some(admitted) =
                    machine.begin(OpName::FilesRead, gate_args, Some(&call.signal))
                {
                    admitted.await?;
                }
                call.signal.check()?;

                let ops: Arc<dyn ReadOperations> = custom_ops.clone().unwrap_or_else(|| {
                    Arc::new(MachineReadOperations {
                        machine: machine.clone(),
                        signal: call.signal.clone(),
                        detect_image_mime_type: detect_image_mime_type.clone(),
                    })
                });
                let base_cwd = call
                    .context
                    .map_or(cwd.as_path(), ToolContext::cwd)
                    .to_path_buf();
                let absolute_path = resolve_read_path(input.path.clone(), base_cwd).await?;
                call.signal.check()?;

                if let Some(policy) = &filesystem_policy {
                    let canonical = canonicalize_path(absolute_path.clone()).await?;
                    check_filesystem_policy(
                        Some(policy),
                        &canonical,
                        FilesystemOperation::Read,
                        "read",
                    )
                    .await?;
                    call.signal.check()?;
                }

                let (mime, buffer) = if let Some(policy) = &execution_policy {
                    let ops = Arc::clone(&ops);
                    let path = absolute_path.clone();
                    policy
                        .run(&call.signal, async move { ops.read_checked(&path).await })
                        .await?
                } else {
                    ops.read_checked(&absolute_path).await?
                };
                call.signal.check()?;

                if let Some(mime) = mime {
                    let original_mime = mime.clone();
                    let resize_options = if context_model {
                        call.context
                            .and_then(ToolContext::model)
                            .and_then(|model| model.input_limits.as_ref())
                            .and_then(|limits| limits.images.as_ref())
                            .and_then(|images| images.resize.as_ref())
                            .or(fallback_resize_options.as_ref())
                            .cloned()
                    } else {
                        fallback_resize_options.clone()
                    };
                    let proc = Arc::clone(&process_image);
                    let (mut images, mut note) = tokio::task::spawn_blocking(move || {
                        proc(buffer, mime, auto_resize_images, resize_options)
                    })
                    .await
                    .map_err(|e| ToolError::Message(e.to_string()))??;
                    if images.is_empty() {
                        note = format!("Read image file [{original_mime}]\n{note}");
                    }
                    if context_model
                        && call.context.and_then(ToolContext::model).is_some_and(|m| {
                            !m.input.contains(&maho_ai::types::InputModality::Image)
                        })
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

                let text_content = String::from_utf8_lossy(&buffer);
                let shaped = shape_text_read(
                    &input.path,
                    &absolute_path,
                    &text_content,
                    input.offset,
                    input.limit,
                    folder.as_ref(),
                )
                .await?;
                Ok(ToolResult {
                    content: shaped.content,
                    details: shaped.details,
                    structured_content: None,
                    is_error: None,
                })
            };

            tokio::select! {
                result = work => result,
                () = call.signal.cancelled() => Err(ToolError::Aborted),
            }
        })
    });

    let mut tool = ToolDefinition::new(
        "read",
        "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to 2000 lines or 50KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to read (relative or absolute)" },
                "offset": { "type": "number", "description": "Line number to start reading from (1-indexed)" },
                "limit": { "type": "number", "description": "Maximum number of lines to read" }
            },
            "required": ["path"]
        }),
        execute,
    );
    tool.workspace_effect = Some(ToolWorkspaceEffect::Read);
    tool.constrained_sampling = Some(maho_ai::types::ConstrainedSampling::Config(
        maho_ai::types::ConstrainedSamplingConfig::JsonSchema {
            strict: maho_ai::types::JsonSchemaStrictness::Prefer,
        },
    ));
    tool.prompt_snippet = Some(READ_PROMPT_SNIPPET.to_owned());
    tool.prompt_guidelines = Some(vec![READ_PROMPT_GUIDELINE.to_owned()]);
    tool
}
