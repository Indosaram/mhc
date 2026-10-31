//! senpi `test/image-processing.test.ts` (pin 903b8336).
use base64::Engine;
use maho_cli::utils::image_convert::convert_to_png;
use maho_cli::utils::image_resize::{
    ImageResizeOptions, ResizedImage, format_dimension_note, resize_image,
};

const TINY_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACAQMAAABIeJ9nAAAAIGNIUk0AAHomAACAhAAA+gAAAIDoAAB1MAAA6mAAADqYAAAXcJy6UTwAAAAGUExURf8AAP///0EdNBEAAAABYktHRAH/Ai3eAAAAB3RJTUUH6gEOADM5Ddoh/wAAAAxJREFUCNdjYGBgAAAABAABJzQnCgAAACV0RVh0ZGF0ZTpjcmVhdGUAMjAyNi0wMS0xNFQwMDo1MTo1NyswMDowMOnKzHgAAAAldEVYdGRhdGU6bW9kaWZ5ADIwMjYtMDEtMTRUMDA6NTE6NTcrMDA6MDCYl3TEAAAAKHRFWHRkYXRlOnRpbWVzdGFtcAAyMDI2LTAxLTE0VDAwOjUxOjU3KzAwOjAwz4JVGwAAAABJRU5ErkJggg==";
const TINY_JPEG: &str = "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAMCAgMCAgMDAwMEAwMEBQgFBQQEBQoHBwYIDAoMDAsKCwsNDhIQDQ4RDgsLEBYQERMUFRUVDA8XGBYUGBIUFRT/2wBDAQMEBAUEBQkFBQkUDQsNFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBT/wAARCAACAAIDAREAAhEBAxEB/8QAFAABAAAAAAAAAAAAAAAAAAAACf/EABQQAQAAAAAAAAAAAAAAAAAAAAD/xAAVAQEBAAAAAAAAAAAAAAAAAAAGCf/EABQRAQAAAAAAAAAAAAAAAAAAAAD/2gAMAwEAAhEDEQA/AD3VTB3/2Q==";
const TINY_JPEG_2X1: &str = "/9j/4AAQSkZJRgABAgAAAQABAAD/wAARCAABAAIDAREAAhEBAxEB/9sAQwADAgIDAgIDAwMDBAMDBAUIBQUEBAUKBwcGCAwKDAwLCgsLDQ4SEA0OEQ4LCxAWEBETFBUVFQwPFxgWFBgSFBUU/9sAQwEDBAQFBAUJBQUJFA0LDRQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQU/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREAAgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3uLm6wsPExcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwD4H8Q/8h/Uv+vmX/0M1/o1wJ/ySWU/9g1D/wBNRMOM/wDkp8z/AOv9b/05I//Z";
const MEDIUM_PNG_100X100: &str = "iVBORw0KGgoAAAANSUhEUgAAAGQAAABkCAAAAABVicqIAAAAAmJLR0QA/4ePzL8AAAAHdElNRQfqAQ4AMzkN2iH/AAAAP0lEQVRo3u3NQQEAAAQEMASXXYrz2gqst/Lm4ZBIJBKJRCKRSCQSiUQikUgkEolEIpFIJBKJRCKRSCQSiSTsAP1cAUZeKtreAAAAJXRFWHRkYXRlOmNyZWF0ZQAyMDI2LTAxLTE0VDAwOjUxOjU3KzAwOjAw6crMeAAAACV0RVh0ZGF0ZTptb2RpZnkAMjAyNi0wMS0xNFQwMDo1MTo1NyswMDowMJiXdMQAAAAodEVYdGRhdGU6dGltZXN0YW1wADIwMjYtMDEtMTRUMDA6NTE6NTcrMDA6MDDPglUbAAAAAElFTkSuQmCC";
const LARGE_PNG_200X200: &str = "iVBORw0KGgoAAAANSUhEUgAAAMgAAADIEAIAAAByquWKAAAAIGNIUk0AAHomAACAhAAA+gAAAIDoAAB1MAAA6mAAADqYAAAXcJy6UTwAAAAGYktHRP///////wlY99wAAAAHdElNRQfqAQ4AMzqU03BFAAAEEElEQVR42u3dvWqUURSF4XXMRAyIhSAWQvwnWtjaWusFx9bW1kKD/wELESxEiBjDjJdwysOC57mCXW32+zGcGbtdACpsdi+T8Xz1GABzm91xMl6sHgNgblzcT/Y+rh4DYG6cv0/2H64eA2BOEgI1fHQHaoyzbXIwVo8BMCcJgRqSEKgxft9Lrn5aPQbA3Ph1klw7Wj0GwJwkBGpYWECN8fMiub63egyAORcWUMPCAmqMH3eTG59XjwEwN76/S24+Wj0GwJwkBGpYWECN8e1fcmuzegyAORcWUMPCAmqM09vJ4dfVYwDMjS9vkzuPV48BMCcJgRqeSAZqjA/nyYP91WMAzElCoIYkBGqMk8Pk6HT1GABzm+3L5JIkBApIQqCGj+5AjfHmb/Lk8uoxAOYkIVBDEgI1LCygxmZ7nFyShEABFxZQw8ICaozXf5KnV1aPATDnwgJqWFhADQsLqOFnDUANFxZQw8ICaoxXZ8mzg9VjAMy5sIAaFhZQw3tYQA1/QgHUkIRADUkI1JCEQA1JCNSQhEANFxZQwzcsoIYkBGpIQqCGJARqSEKghiQEalhYQA1/QgHUcGEBNSwsoIYkBGq4sIAaFhZQw8ICaviGBdRwYQE1LCyghiQEariwgBoWFlDDe1hADS+OAjUkIVBDEgI1JCFQQxICNSQhUMOFBdTwDQuoIQmBGpIQqCEJgRqSEKghCYEaFhZQw4ujQA0XFlDDwgJqSEKghgsLqGFhATUsLKCGb1hADRcWUMPCAmpIQqCGCwuoYWEBNbyHBdTw4ihQQxICNSQhUEMSAjUkIVBDEgI1XFhADd+wgBqSEKghCYEakhCoIQmBGpIQqGFhATW8OArUcGEBNSwsoIYkBGq4sIAaFhZQw8ICaviGBdRwYQE1LCyghiQEariwgBoWFlDDe1hADS+OAjUkIVBDEgI1JCFQQxICNSQhUMOFBdTwDQuoIQmBGpIQqCEJgRqSEKghCYEa49lB8ups9RgAc14cBWpIQqCGhQXUkIRADRcWUMPCAmqMp1eS139WjwEw5xsWUEMSAjUsLKCGJARquLCAGhYWUGM8uZy8+bt6DIA5L44CNSQhUMMTyUCNcXSanByuHgNgThICNSQhUGM82E8+nK8eA2DOzxqAGpIQqDHuPE6+vF09BsDcOPyanN5ePQbAnCQEavgdFlBj3Nok3/6tHgNgzoujQA1JCNQYNx8l39+tHgNgbtz4nPy4u3oMgDlJCNSwsIAa4/pe8vNi9RgAc37WANSQhECNce0o+XWyegyAuXH1U/L73uoxAOYkIVDDwgJqjIORnG1XjwEw508ogBqSEKgx9h8m5+9XjwEwN/Y+Jhf3V48BMCcJgRpjPE+2x6vHAJgbSbLbrR4DYO4/GqiSgXN+ksgAAAAldEVYdGRhdGU6Y3JlYXRlADIwMjYtMDEtMTRUMDA6NTE6NTcrMDA6MDDpysx4AAAAJXRFWHRkYXRlOm1vZGlmeQAyMDI2LTAxLTE0VDAwOjUxOjU3KzAwOjAwmJd0xAAAACh0RVh0ZGF0ZTp0aW1lc3RhbXAAMjAyNi0wMS0xNFQwMDo1MTo1NyswMDowMM+CVRsAAAAASUVORK5CYII=";

fn bytes(data: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .expect("fixture base64")
}

fn limits(max_width: u32, max_height: u32, max_bytes: f64) -> ImageResizeOptions {
    ImageResizeOptions {
        max_width,
        max_height,
        max_bytes,
        ..Default::default()
    }
}

#[test]
fn convert_to_png_returns_original_data_for_png_input() {
    let result = convert_to_png(TINY_PNG, "image/png").expect("png passthrough");
    assert_eq!(result.data, TINY_PNG);
    assert_eq!(result.mime_type, "image/png");
}

#[test]
fn convert_to_png_converts_jpeg_to_png() {
    let result = convert_to_png(TINY_JPEG, "image/jpeg").expect("jpeg converts");
    assert_eq!(result.mime_type, "image/png");
    let png = bytes(&result.data);
    assert_eq!(&png[..4], b"\x89PNG");
}

fn app1_segment(payload: &[u8]) -> Vec<u8> {
    let mut segment = vec![0xff, 0xe1];
    segment.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    segment.extend_from_slice(payload);
    segment
}

fn jpeg_with_xmp_before_orientation() -> String {
    let jpeg = bytes(TINY_JPEG_2X1);
    let xmp =
        app1_segment(b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>");
    let mut exif = b"Exif\0\0".to_vec();
    exif.extend_from_slice(&[
        0x49, 0x49, 0x2a, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x12, 0x01, 0x03, 0x00, 0x01,
        0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ]);
    let orientation6 = app1_segment(&exif);
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&xmp);
    out.extend_from_slice(&orientation6);
    out.extend_from_slice(&jpeg[2..]);
    base64::engine::general_purpose::STANDARD.encode(out)
}

#[test]
fn convert_to_png_applies_exif_orientation_after_an_xmp_app1_segment() {
    let result =
        convert_to_png(&jpeg_with_xmp_before_orientation(), "image/jpeg").expect("jpeg converts");
    let png = bytes(&result.data);
    assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 1);
    assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 2);
}

#[tokio::test]
async fn resize_image_keeps_caller_input_bytes_intact() {
    let input = bytes(TINY_PNG);
    let original = input.clone();
    assert!(
        resize_image(&input, "image/png", limits(100, 100, 1024.0 * 1024.0))
            .await
            .is_some()
    );
    assert_eq!(input, original);
}

#[tokio::test]
async fn resize_image_returns_original_image_if_within_limits() {
    let result = resize_image(
        &bytes(TINY_PNG),
        "image/png",
        limits(100, 100, 1024.0 * 1024.0),
    )
    .await
    .expect("within limits");
    assert!(!result.was_resized);
    assert_eq!(result.data, TINY_PNG);
    assert_eq!((result.original_width, result.original_height), (2, 2));
    assert_eq!((result.width, result.height), (2, 2));
}

#[tokio::test]
async fn resize_image_resizes_image_exceeding_dimension_limits() {
    let result = resize_image(
        &bytes(MEDIUM_PNG_100X100),
        "image/png",
        limits(50, 50, 1024.0 * 1024.0),
    )
    .await
    .expect("resized");
    assert!(result.was_resized);
    assert_eq!((result.original_width, result.original_height), (100, 100));
    assert!(result.width <= 50);
    assert!(result.height <= 50);
}

#[tokio::test]
async fn resize_image_resizes_image_exceeding_byte_limit() {
    let original_size = bytes(LARGE_PNG_200X200).len();
    let max_bytes = (LARGE_PNG_200X200.len() as f64 * 0.9).floor();
    let result = resize_image(
        &bytes(LARGE_PNG_200X200),
        "image/png",
        limits(2000, 2000, max_bytes),
    )
    .await
    .expect("reduced");
    assert!(bytes(&result.data).len() < original_size);
    assert!(result.data.len() < LARGE_PNG_200X200.len());
}

#[tokio::test]
async fn resize_image_returns_none_when_image_cannot_be_resized_below_max_bytes() {
    assert!(
        resize_image(
            &bytes(LARGE_PNG_200X200),
            "image/png",
            limits(2000, 2000, 1.0)
        )
        .await
        .is_none()
    );
}

#[tokio::test]
async fn resize_image_handles_jpeg_input() {
    let result = resize_image(
        &bytes(TINY_JPEG),
        "image/jpeg",
        limits(100, 100, 1024.0 * 1024.0),
    )
    .await
    .expect("jpeg");
    assert!(!result.was_resized);
    assert_eq!((result.original_width, result.original_height), (2, 2));
}

fn resized(original: (u32, u32), shown: (u32, u32), was_resized: bool) -> ResizedImage {
    ResizedImage {
        data: String::new(),
        mime_type: "image/png".to_owned(),
        original_width: original.0,
        original_height: original.1,
        width: shown.0,
        height: shown.1,
        was_resized,
    }
}

#[test]
fn format_dimension_note_returns_none_for_non_resized_images() {
    assert_eq!(
        format_dimension_note(&resized((100, 100), (100, 100), false)),
        None
    );
}

#[test]
fn format_dimension_note_returns_formatted_note_for_resized_images() {
    let note = format_dimension_note(&resized((2000, 1000), (1000, 500), true)).expect("note");
    assert!(note.contains("original 2000x1000"));
    assert!(note.contains("displayed at 1000x500"));
    assert!(note.contains("2.00"));
}
