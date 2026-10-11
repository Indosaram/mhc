use maho_ai::types::{ContentBlock, ImageContent};
pub async fn normalize_tool_result_images(content: &[ContentBlock], auto_resize_images: Option<bool>) -> std::borrow::Cow<'_, [ContentBlock]> {
    if !content.iter().any(|block| matches!(block, ContentBlock::Image(_))) { return std::borrow::Cow::Borrowed(content); }
    let mut normalized = Vec::new(); let mut changed = false;
    for block in content { let ContentBlock::Image(image) = block else { normalized.push(block.clone()); continue; }; let bytes = super::image_convert::decode_base64(&image.data); let Ok(processed) = super::image_process::process_image(&bytes, &image.mime_type, super::image_process::ProcessImageOptions { auto_resize_images, ..Default::default() }).await else { normalized.push(block.clone()); continue; }; if processed.data == image.data && processed.mime_type == image.mime_type && processed.hints.is_empty() { normalized.push(block.clone()); continue; } normalized.push(ContentBlock::Image(ImageContent { data: processed.data, mime_type: processed.mime_type })); if !processed.hints.is_empty() { normalized.push(ContentBlock::text(processed.hints.join("\n"))); } changed = true; }
    if changed { std::borrow::Cow::Owned(normalized) } else { std::borrow::Cow::Borrowed(content) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;

    fn png_chunk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut chunk = Vec::with_capacity(body.len() + 12);
        chunk.extend_from_slice(&(body.len() as u32).to_be_bytes());
        chunk.extend_from_slice(kind);
        chunk.extend_from_slice(body);
        let mut crc = !0_u32;
        for byte in kind.iter().chain(body) {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb8_8320 & (0_u32.wrapping_sub(crc & 1)));
            }
        }
        chunk.extend_from_slice(&(!crc).to_be_bytes());
        chunk
    }

    fn create_png(width: u32, height: u32) -> Vec<u8> {
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);
        let mut raw = Vec::with_capacity((width as usize + 1) * height as usize);
        for row in 0..height {
            raw.push(0);
            raw.extend(std::iter::repeat_n((row % 256) as u8, width as usize));
        }
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&raw).expect("compress test PNG");
        let compressed = encoder.finish().expect("finish test PNG");
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend(png_chunk(b"IHDR", &ihdr));
        png.extend(png_chunk(b"IDAT", &compressed));
        png.extend(png_chunk(b"IEND", &[]));
        png
    }

    fn create_tiny_bmp_1x1_red_24bpp() -> Vec<u8> {
        let mut bmp = vec![0; 58];
        bmp[0..2].copy_from_slice(b"BM");
        bmp[2..6].copy_from_slice(&58_u32.to_le_bytes());
        bmp[10..14].copy_from_slice(&54_u32.to_le_bytes());
        bmp[14..18].copy_from_slice(&40_u32.to_le_bytes());
        bmp[18..22].copy_from_slice(&1_i32.to_le_bytes());
        bmp[22..26].copy_from_slice(&1_i32.to_le_bytes());
        bmp[26..28].copy_from_slice(&1_u16.to_le_bytes());
        bmp[28..30].copy_from_slice(&24_u16.to_le_bytes());
        bmp[34..38].copy_from_slice(&4_u32.to_le_bytes());
        bmp[56] = 0xff;
        bmp
    }

    fn image_block(bytes: &[u8], mime_type: &str) -> ContentBlock {
        ContentBlock::Image(ImageContent {
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            mime_type: mime_type.to_owned(),
        })
    }

    fn png_dimensions(data: &str) -> (u32, u32) {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .expect("normalized image is base64");
        (
            u32::from_be_bytes(bytes[16..20].try_into().expect("PNG width")),
            u32::from_be_bytes(bytes[20..24].try_into().expect("PNG height")),
        )
    }

    #[tokio::test]
    async fn leaves_non_image_content_borrowed() {
        let content = vec![ContentBlock::text("unchanged")];
        assert!(matches!(normalize_tool_result_images(&content, None).await, std::borrow::Cow::Borrowed(_)));
    }

    #[tokio::test]
    async fn keeps_images_within_limits_borrowed() {
        let content = vec![image_block(&create_png(1, 1), "image/png")];
        assert!(matches!(normalize_tool_result_images(&content, None).await, std::borrow::Cow::Borrowed(_)));
    }

    #[tokio::test]
    async fn resizes_oversized_images_and_notes_original_dimensions() {
        let content = vec![image_block(&create_png(2400, 4800), "image/png")];
        let normalized = normalize_tool_result_images(&content, None).await;
        assert!(matches!(normalized, std::borrow::Cow::Owned(_)));
        assert_eq!(normalized.len(), 2);
        let ContentBlock::Image(image) = &normalized[0] else { panic!("first block should be image"); };
        let (width, height) = png_dimensions(&image.data);
        assert!(width <= 2000);
        assert!(height <= 2000);
        let ContentBlock::Text(note) = &normalized[1] else { panic!("second block should be note"); };
        assert!(note.text.contains("original 2400x4800"));
    }

    #[tokio::test]
    async fn leaves_oversized_images_alone_when_resize_is_disabled() {
        let content = vec![image_block(&create_png(2400, 4800), "image/png")];
        assert!(matches!(normalize_tool_result_images(&content, Some(false)).await, std::borrow::Cow::Borrowed(_)));
    }

    #[tokio::test]
    async fn converts_bmp_when_resize_is_disabled() {
        let content = vec![image_block(&create_tiny_bmp_1x1_red_24bpp(), "image/bmp")];
        let normalized = normalize_tool_result_images(&content, Some(false)).await;
        assert!(matches!(normalized, std::borrow::Cow::Owned(_)));
        assert!(matches!(&normalized[0], ContentBlock::Image(image) if image.mime_type == "image/png"));
        assert!(matches!(&normalized[1], ContentBlock::Text(note) if note.text == "[Image converted from image/bmp to image/png.]"));
    }

    #[tokio::test]
    async fn keeps_undecodable_images_instead_of_dropping_tool_output() {
        let content = vec![ContentBlock::Image(ImageContent {
            data: "bm90LWFuLWltYWdl".to_owned(),
            mime_type: "image/png".to_owned(),
        })];
        assert!(matches!(normalize_tool_result_images(&content, None).await, std::borrow::Cow::Borrowed(_)));
    }

    #[tokio::test]
    async fn preserves_surrounding_text_blocks_and_order() {
        let content = vec![
            ContentBlock::text("before"),
            image_block(&create_png(2400, 100), "image/png"),
            ContentBlock::text("after"),
        ];
        let normalized = normalize_tool_result_images(&content, None).await;
        assert!(matches!(
            normalized.iter().map(|block| match block { ContentBlock::Text(_) => "text", ContentBlock::Image(_) => "image", _ => "other" }).collect::<Vec<_>>().as_slice(),
            ["text", "image", "text", "text"]
        ));
        assert!(matches!(&normalized[0], ContentBlock::Text(text) if text.text == "before"));
        assert!(matches!(&normalized[3], ContentBlock::Text(text) if text.text == "after"));
    }
}
