use maho_ai::types::{ContentBlock, ImageContent};
pub async fn normalize_tool_result_images(content: &[ContentBlock], auto_resize_images: Option<bool>) -> std::borrow::Cow<'_, [ContentBlock]> {
    if !content.iter().any(|block| matches!(block, ContentBlock::Image(_))) { return std::borrow::Cow::Borrowed(content); }
    let mut normalized = Vec::new();
    let mut changed = false;
    for block in content {
        let ContentBlock::Image(image) = block else { normalized.push(block.clone()); continue; };
        let bytes = super::image_convert::decode_base64(&image.data);
        let Ok(processed) = super::image_process::process_image(&bytes, &image.mime_type, super::image_process::ProcessImageOptions { auto_resize_images, ..Default::default() }).await else { normalized.push(block.clone()); continue; };
        if processed.data == image.data && processed.mime_type == image.mime_type && processed.hints.is_empty() { normalized.push(block.clone()); continue; }
        normalized.push(ContentBlock::Image(ImageContent { data: processed.data, mime_type: processed.mime_type }));
        if !processed.hints.is_empty() { normalized.push(ContentBlock::text(processed.hints.join("\n"))); }
        changed = true;
    }
    if changed { std::borrow::Cow::Owned(normalized) } else { std::borrow::Cow::Borrowed(content) }
}
