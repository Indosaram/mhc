use maho_ai::types::ImageContent;
use std::path::Path;
pub struct ProcessedFiles { pub text: String, pub images: Vec<ImageContent> }
pub async fn process_file_arguments(file_args: &[String], cwd: &Path, auto_resize_images: Option<bool>) -> Result<ProcessedFiles, String> {
    let mut text = String::new();
    let mut images = Vec::new();
    for file in file_args {
        let path = maho_tools::path_utils::resolve_read_path_async(file, cwd).await;
        let metadata = tokio::fs::metadata(&path).await.map_err(|_| format!("Error: File not found: {}", path.display()))?;
        if metadata.len() == 0 { continue; }
        let mime = crate::utils::mime::detect_supported_image_mime_type_from_file(&path).map_err(|error| error.to_string())?;
        let bytes = tokio::fs::read(&path).await.map_err(|error| format!("Error: Could not read file {}: {error}", path.display()))?;
        if let Some(mime) = mime {
            match crate::utils::image_process::process_image(&bytes, mime, crate::utils::image_process::ProcessImageOptions { auto_resize_images, ..Default::default() }).await {
                Ok(processed) => {
                    images.push(ImageContent { data: processed.data, mime_type: processed.mime_type });
                    text.push_str(&format!("<file name=\"{}\">{}</file>\n", path.display(), processed.hints.join("\n")));
                }
                Err(message) => text.push_str(&format!("<file name=\"{}\">{message}</file>\n", path.display())),
            }
        } else {
            let content = String::from_utf8_lossy(&bytes);
            let content = content.strip_prefix('\u{feff}').unwrap_or(&content);
            text.push_str(&format!("<file name=\"{}\">\n{content}\n</file>\n", path.display()));
        }
    }
    Ok(ProcessedFiles { text, images })
}
