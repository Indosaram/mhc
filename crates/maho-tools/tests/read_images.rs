use maho_tools::{definition::*, read::*};
use base64::Engine;
use serde_json::json;

async fn read_image(bytes: Vec<u8>, extension: &str, resize: bool) -> Result<ToolResult, ToolError> {
    let directory = tempfile::tempdir()?;
    let name = format!("image.{extension}");
    std::fs::write(directory.path().join(&name), bytes)?;
    let tool = create_read_tool_definition(directory.path().into(), ReadToolOptions { auto_resize_images: Some(resize), ..Default::default() });
    (tool.execute)(ToolCall { id:"image", params:json!({"path":name}), signal:AbortSignal::default(), on_update:None, context:None }).await
}

#[tokio::test]
async fn read_preserves_small_png_bytes() {
    let image = image::DynamicImage::new_rgb8(2, 3);
    let mut encoded = std::io::Cursor::new(Vec::new());
    image.write_to(&mut encoded, image::ImageFormat::Png).unwrap();
    let bytes = encoded.into_inner();
    let result = read_image(bytes.clone(), "png", true).await.unwrap();
    match &result.content[1] {
        ToolContent::Image { data, mime_type } => {
            assert_eq!(mime_type, "image/png");
            assert_eq!(base64::engine::general_purpose::STANDARD.decode(data).unwrap(), bytes);
        }
        other => panic!("unexpected content {other:?}"),
    }
}

#[tokio::test]
async fn read_converts_bmp_to_inline_png() {
    let image = image::DynamicImage::new_rgb8(1, 1);
    let mut encoded = std::io::Cursor::new(Vec::new());
    image.write_to(&mut encoded, image::ImageFormat::Bmp).unwrap();
    let result = read_image(encoded.into_inner(), "bmp", false).await.unwrap();
    match &result.content[1] {
        ToolContent::Image { data, mime_type } => {
            assert_eq!(mime_type, "image/png");
            let bytes = base64::engine::general_purpose::STANDARD.decode(data).unwrap();
            assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 1);
        }
        other => panic!("unexpected content {other:?}"),
    }
}

#[tokio::test]
async fn read_resizes_wide_image_to_dimension_limit() {
    let image = image::DynamicImage::new_rgb8(3000, 30);
    let mut encoded = std::io::Cursor::new(Vec::new());
    image.write_to(&mut encoded, image::ImageFormat::Png).unwrap();
    let result = read_image(encoded.into_inner(), "png", true).await.unwrap();
    match &result.content[1] {
        ToolContent::Image { data, .. } => {
            let decoded = image::load_from_memory(&base64::engine::general_purpose::STANDARD.decode(data).unwrap()).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (2000, 20));
        }
        other => panic!("unexpected content {other:?}"),
    }
}
