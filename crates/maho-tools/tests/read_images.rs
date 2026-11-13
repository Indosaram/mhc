use base64::Engine;
use maho_tools::{definition::*, read::*};
use serde_json::json;

async fn read_image(
    bytes: Vec<u8>,
    extension: &str,
    resize: bool,
) -> Result<ToolResult, ToolError> {
    let directory = tempfile::tempdir()?;
    let name = format!("image.{extension}");
    std::fs::write(directory.path().join(&name), bytes)?;
    let tool = create_read_tool_definition(
        directory.path().into(),
        ReadToolOptions {
            auto_resize_images: Some(resize),
            ..Default::default()
        },
    );
    (tool.execute)(ToolCall {
        id: "image",
        params: json!({"path":name}),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    })
    .await
}

#[tokio::test]
async fn read_preserves_small_png_bytes() {
    let image = image::DynamicImage::new_rgb8(2, 3);
    let mut encoded = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    let bytes = encoded.into_inner();
    let result = read_image(bytes.clone(), "png", true).await.unwrap();
    match &result.content[1] {
        ToolContent::Image { data, mime_type } => {
            assert_eq!(mime_type, "image/png");
            assert_eq!(
                base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .unwrap(),
                bytes
            );
        }
        other => panic!("unexpected content {other:?}"),
    }
}

#[tokio::test]
async fn read_converts_bmp_to_inline_png() {
    let image = image::DynamicImage::new_rgb8(1, 1);
    let mut encoded = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut encoded, image::ImageFormat::Bmp)
        .unwrap();
    let result = read_image(encoded.into_inner(), "bmp", false)
        .await
        .unwrap();
    match &result.content[1] {
        ToolContent::Image { data, mime_type } => {
            assert_eq!(mime_type, "image/png");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .unwrap();
            assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 1);
        }
        other => panic!("unexpected content {other:?}"),
    }
}

#[tokio::test]
async fn read_resizes_wide_image_to_dimension_limit() {
    let image = image::DynamicImage::new_rgb8(3000, 30);
    let mut encoded = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    let result = read_image(encoded.into_inner(), "png", true).await.unwrap();
    match &result.content[1] {
        ToolContent::Image { data, .. } => {
            let decoded = image::load_from_memory(
                &base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!((decoded.width(), decoded.height()), (2000, 20));
        }
        other => panic!("unexpected content {other:?}"),
    }
}

/// Photo-like pixels: a smooth gradient plus per-pixel noise. PNG must store the noise, JPEG
/// quantizes it away, so the JPEG candidate of these pixels is far smaller at the same dimensions.
fn noisy_rgb(width: u32, height: u32) -> image::RgbImage {
    image::RgbImage::from_fn(width, height, |x, y| {
        let noise = x
            .wrapping_mul(73_856_093)
            .wrapping_add(y.wrapping_mul(19_349_663))
            .wrapping_mul(2_246_822_519)
            .rotate_left(7);
        let noise = u8::try_from(noise & 0x3F).expect("byte");
        let base = |scale: u32| u8::try_from(((x + y) / scale) % 96).expect("byte");
        image::Rgb([
            base(3).saturating_add(noise),
            base(5).saturating_add(noise),
            base(7).saturating_add(noise),
        ])
    })
}

/// The same noise with a varying alpha channel, so JPEG would visibly flatten it.
fn noisy_rgba(width: u32, height: u32) -> image::RgbaImage {
    let rgb = noisy_rgb(width, height);
    image::RgbaImage::from_fn(width, height, |x, y| {
        let pixel = rgb.get_pixel(x, y);
        image::Rgba([
            pixel[0],
            pixel[1],
            pixel[2],
            u8::try_from((x + y) % 256).expect("byte"),
        ])
    })
}

fn png_bytes(image: &image::DynamicImage) -> Vec<u8> {
    let mut encoded = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut encoded, image::ImageFormat::Png)
        .expect("encode png fixture");
    encoded.into_inner()
}

fn jpeg_bytes(image: &image::DynamicImage, quality: u8) -> Vec<u8> {
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, quality)
        .encode_image(&image.to_rgb8())
        .expect("encode jpeg fixture");
    encoded
}

#[tokio::test]
async fn read_keeps_small_png_out_of_the_resize_path() {
    // Given a 2x3 PNG well below every limit.
    let bytes = png_bytes(&image::DynamicImage::new_rgb8(2, 3));
    // When resize is enabled.
    let result = read_image(bytes.clone(), "png", true).await.unwrap();
    // Then the bytes are untouched and no candidate selection note is emitted.
    match &result.content[1] {
        ToolContent::Image { data, mime_type } => {
            assert_eq!(mime_type, "image/png");
            assert_eq!(
                base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .unwrap(),
                bytes
            );
        }
        other => panic!("unexpected content {other:?}"),
    }
    match &result.content[0] {
        ToolContent::Text { text, .. } => assert_eq!(text, "Read image file [image/png]"),
        other => panic!("unexpected content {other:?}"),
    }
}

#[tokio::test]
async fn read_prefers_smaller_jpeg_candidate_over_fitting_png() {
    // Given an opaque 2400x360 PNG whose PNG candidate fits the inline budget while its JPEG
    // candidate at the same displayed dimensions is several times smaller.
    let fixture = image::DynamicImage::ImageRgb8(noisy_rgb(2400, 360));
    let resized = fixture.resize_exact(2000, 300, image::imageops::FilterType::Lanczos3);
    let png_base64_len = png_bytes(&resized).len().div_ceil(3) * 4;
    assert!(
        png_base64_len < 4_718_592,
        "fixture must let the PNG candidate fit the default budget, got {png_base64_len}"
    );
    let q80 = jpeg_bytes(&resized, 80);
    assert!(
        q80.len().div_ceil(3) * 4 < png_base64_len,
        "fixture must let the q80 jpeg beat the png candidate: {} vs {png_base64_len}",
        q80.len().div_ceil(3) * 4
    );
    // When the resize branch runs under the default policy.
    let result = read_image(png_bytes(&fixture), "png", true).await.unwrap();
    // Then the smaller candidate wins, keeping the displayed dimensions unchanged.
    match &result.content[1] {
        ToolContent::Image { data, mime_type } => {
            assert_eq!(mime_type, "image/jpeg");
            assert!(
                data.len() < png_base64_len,
                "jpeg payload {} must be smaller than the png candidate {png_base64_len}",
                data.len()
            );
            // And it is the highest-quality JPEG that fits, not the smallest JPEG candidate: the
            // q40 candidate is smaller than this one.
            let payload = base64::engine::general_purpose::STANDARD
                .decode(data)
                .unwrap();
            assert!(
                payload == q80,
                "payload must be the q80 encoding: {} bytes vs expected {} bytes",
                payload.len(),
                q80.len()
            );
            assert!(
                payload.len() > jpeg_bytes(&resized, 40).len(),
                "payload must not be the smallest (q40) jpeg candidate"
            );
            let decoded = image::load_from_memory(&payload).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (2000, 300));
        }
        other => panic!("unexpected content {other:?}"),
    }
}

#[tokio::test]
async fn read_keeps_png_for_alpha_image_that_must_be_resized() {
    // Given a 2400x360 RGBA image whose alpha channel JPEG would flatten, with content the JPEG
    // candidate encodes more compactly than PNG, so only the alpha rule can preserve PNG.
    let fixture = image::DynamicImage::ImageRgba8(noisy_rgba(2400, 360));
    let resized = fixture.resize_exact(2000, 300, image::imageops::FilterType::Lanczos3);
    let png = png_bytes(&resized);
    let jpeg = jpeg_bytes(&resized, 80);
    assert!(
        jpeg.len() < png.len(),
        "fixture must let the jpeg candidate beat png on size: {} vs {}",
        jpeg.len(),
        png.len()
    );
    assert!(png.len().div_ceil(3) * 4 < 4_718_592);
    // When the width bound forces a resize.
    let result = read_image(png_bytes(&fixture), "png", true).await.unwrap();
    // Then the transparent image stays PNG instead of being flattened to the smaller JPEG.
    match &result.content[1] {
        ToolContent::Image { data, mime_type } => {
            assert_eq!(mime_type, "image/png");
            let payload = base64::engine::general_purpose::STANDARD
                .decode(data)
                .unwrap();
            assert!(
                payload == png,
                "payload must be the png candidate: {} bytes vs expected {} bytes",
                payload.len(),
                png.len()
            );
            let decoded = image::load_from_memory(&payload).unwrap();
            assert!(decoded.color().has_alpha());
            assert_eq!((decoded.width(), decoded.height()), (2000, 300));
        }
        other => panic!("unexpected content {other:?}"),
    }
}
