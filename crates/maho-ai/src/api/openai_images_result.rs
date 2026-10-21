//! Port of senpi packages/ai/src/api/openai-images-result.ts.

use base64::Engine;
use serde_json::Value;

use crate::types::ImageContent;

const MAX_IMAGE_BYTES: usize = 24 * 1024 * 1024;
const PNG_MAGIC: &[u8] = &[0x89, 0x50, 0x4e, 0x47];
const JPEG_MAGIC: &[u8] = &[0xff, 0xd8, 0xff];
const WEBP_MAGIC: &[u8] = &[0x52, 0x49, 0x46, 0x46, 0x57, 0x45, 0x42, 0x50];

/// The container the request asked for; a replaced payload without one falls back to the API default.
pub fn requested_output_format(value: Option<&str>) -> String {
    match value {
        Some("jpeg") => "jpeg".into(),
        Some("webp") => "webp".into(),
        _ => "png".into(),
    }
}

/// Decodes one response datum. Inline base64 is labeled by its magic bytes, falling back to the
/// requested container only when the header is unrecognizable: gateways can ignore `output_format`
/// and return png, and a wrong label would follow the file to disk.
pub async fn resolve_image(
    datum: &Value,
    output_format: &str,
    options: Option<&crate::types::ImagesOptions>,
) -> Result<ImageContent, String> {
    let b64 = datum
        .get("b64_json")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(b64) = b64 {
        let head = base64_decode(&b64.chars().take(24).collect::<String>()).unwrap_or_default();
        let mime_type = detect_mime(&head)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("image/{output_format}"));
        return Ok(ImageContent {
            data: b64.to_owned(),
            mime_type,
        });
    }
    let url = datum
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(url) = url else {
        return Err("OpenAI images response datum contained no image data".into());
    };
    if url.starts_with("data:") {
        return parse_data_url(url);
    }
    hydrate_image_url(url, options).await
}

fn parse_data_url(url: &str) -> Result<ImageContent, String> {
    let Some(rest) = url.strip_prefix("data:") else {
        return Err("OpenAI images response contained an invalid data URL".into());
    };
    let Some((header, data)) = rest.split_once(',') else {
        return Err("OpenAI images response contained an invalid data URL".into());
    };
    let Some((header, encoding)) = header.split_once(';') else {
        return Err("OpenAI images response contained an invalid data URL".into());
    };
    if header.is_empty()
        || !encoding.eq_ignore_ascii_case("base64")
        || data.is_empty()
        || header.contains(',')
        || data.contains(['\r', '\n', '\u{2028}', '\u{2029}'])
    {
        return Err("OpenAI images response contained an invalid data URL".into());
    }
    let mime_type = supported_mime(Some(header))
        .ok_or_else(|| format!("OpenAI images response used unsupported MIME type: {header}"))?;
    Ok(ImageContent {
        data: data.to_owned(),
        mime_type: mime_type.to_owned(),
    })
}

async fn hydrate_image_url(
    url: &str,
    options: Option<&crate::types::ImagesOptions>,
) -> Result<ImageContent, String> {
    let parsed = url::Url::parse(url)
        .map_err(|_| "OpenAI images response URL must be absolute".to_owned())?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("OpenAI images response URL must use HTTP or HTTPS".into());
    }
    let client = options
        .and_then(|options| options.request.fetch.clone())
        .unwrap_or_default();
    let signal = options.and_then(|options| options.request.signal.as_ref());
    let request = client.get(url).send();
    let response = match signal {
        Some(signal) => tokio::select! {
            biased;
            () = signal.cancelled() => return Err("Request aborted".into()),
            response = request => response,
        },
        None => request.await,
    }
    .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "OpenAI image hydration failed with HTTP {}",
            response.status().as_u16()
        ));
    }
    if let Some(declared) = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        && declared > MAX_IMAGE_BYTES
    {
        return Err(oversized_image_error());
    }
    let declared_mime = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .and_then(|value| supported_mime(Some(value)))
        .map(str::to_owned);
    let body = response.bytes();
    let bytes = match signal {
        Some(signal) => tokio::select! {
            biased;
            () = signal.cancelled() => return Err("Request aborted".into()),
            bytes = body => bytes,
        },
        None => body.await,
    }
    .map_err(|error| error.to_string())?
    .to_vec();
    if bytes.is_empty() {
        return Err("OpenAI image hydration returned an empty body".into());
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(oversized_image_error());
    }
    let detected_mime = detect_mime(&bytes).map(str::to_owned);
    if let (Some(declared), Some(detected)) = (&declared_mime, &detected_mime)
        && declared != detected
    {
        return Err(format!(
            "OpenAI image MIME mismatch: declared {declared}, detected {detected}"
        ));
    }
    let mime_type = detected_mime
        .or(declared_mime)
        .ok_or_else(|| "OpenAI image hydration returned unsupported image content".to_owned())?;
    Ok(ImageContent {
        data: base64_encode(&bytes),
        mime_type,
    })
}

fn supported_mime(value: Option<&str>) -> Option<&'static str> {
    match value.map(str::trim).map(str::to_lowercase).as_deref() {
        Some("image/png") => Some("image/png"),
        Some("image/jpeg") => Some("image/jpeg"),
        Some("image/webp") => Some("image/webp"),
        _ => None,
    }
}

fn detect_mime(bytes: &[u8]) -> Option<&'static str> {
    if has_magic(bytes, PNG_MAGIC, 0) {
        return Some("image/png");
    }
    if has_magic(bytes, JPEG_MAGIC, 0) {
        return Some("image/jpeg");
    }
    if has_magic(bytes, &WEBP_MAGIC[..4], 0) && has_magic(bytes, &WEBP_MAGIC[4..], 8) {
        return Some("image/webp");
    }
    None
}

fn has_magic(bytes: &[u8], magic: &[u8], offset: usize) -> bool {
    magic
        .iter()
        .enumerate()
        .all(|(index, value)| bytes.get(offset + index) == Some(value))
}

fn base64_decode(value: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(value).ok()
}

fn base64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn oversized_image_error() -> String {
    "OpenAI image hydration exceeded the 24 MiB limit".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn png_b64() -> String {
        let mut bytes = PNG_MAGIC.to_vec();
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        base64_encode(&bytes)
    }

    #[test]
    fn data_url_accepts_uppercase_encoding_when_header_is_valid() {
        // Given an uppercase encoding marker accepted by upstream's regex.
        let url = "data:IMAGE/PNG;BASE64,AQ==";
        // When parsing the image data URL.
        let image = parse_data_url(url).expect("image");
        // Then MIME normalization preserves the encoded data.
        assert_eq!(
            image,
            ImageContent {
                data: "AQ==".into(),
                mime_type: "image/png".into()
            }
        );
    }

    #[tokio::test]
    async fn hydration_is_cancelled_when_waiting_for_headers_or_body() {
        use crate::utils::abort::AbortController;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for send_headers in [false, true] {
            // Given a server paused at either the response headers or body boundary.
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("listener");
            let url = format!("http://{}/image", listener.local_addr().expect("address"));
            let controller = AbortController::new();
            let (reached, boundary) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut buffer = Vec::new();
                loop {
                    let mut byte = [0];
                    socket.read_exact(&mut byte).await.expect("request headers");
                    buffer.push(byte[0]);
                    if buffer.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                if send_headers {
                    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nContent-Type: image/png\r\n\r\n").await.expect("headers");
                }
                reached.send(()).expect("boundary receiver");
                std::future::pending::<()>().await;
            });
            let mut options = crate::types::ImagesOptions::default();
            options.request.signal = Some(controller.signal());
            let hydration =
                tokio::spawn(async move { hydrate_image_url(&url, Some(&options)).await });
            tokio::time::timeout(std::time::Duration::from_secs(5), boundary)
                .await
                .expect("boundary timeout")
                .expect("boundary");
            // When cancellation is signaled while the server is paused.
            controller.abort(None);
            let result = tokio::time::timeout(std::time::Duration::from_secs(5), hydration)
                .await
                .expect("cancellation timeout")
                .expect("hydration task");
            // Then hydration exits without waiting for the server to finish.
            assert!(result.is_err());
            server.abort();
            assert!(server.await.expect_err("cancelled server").is_cancelled());
        }
    }

    #[test]
    fn requested_output_format_defaults_to_png() {
        assert_eq!(requested_output_format(Some("jpeg")), "jpeg");
        assert_eq!(requested_output_format(Some("webp")), "webp");
        assert_eq!(requested_output_format(Some("png")), "png");
        assert_eq!(requested_output_format(None), "png");
        assert_eq!(requested_output_format(Some("gif")), "png");
    }

    #[tokio::test]
    async fn inline_base64_is_labeled_by_magic_bytes() {
        let image = resolve_image(&json!({ "b64_json": png_b64() }), "webp", None)
            .await
            .expect("image");
        assert_eq!(image.mime_type, "image/png");

        let unknown = base64_encode(b"not an image at all, honestly");
        let image = resolve_image(&json!({ "b64_json": unknown }), "webp", None)
            .await
            .expect("image");
        assert_eq!(image.mime_type, "image/webp");
    }

    #[tokio::test]
    async fn data_urls_are_parsed_and_unsupported_types_rejected() {
        let image = resolve_image(
            &json!({ "url": format!("data:image/png;base64,{}", png_b64()) }),
            "png",
            None,
        )
        .await
        .expect("image");
        assert_eq!(image.mime_type, "image/png");

        assert_eq!(
            resolve_image(
                &json!({ "url": "data:image/tiff;base64,AAAA" }),
                "png",
                None
            )
            .await,
            Err("OpenAI images response used unsupported MIME type: image/tiff".into())
        );
        assert_eq!(
            resolve_image(&json!({ "url": "data:image/png,AAAA" }), "png", None).await,
            Err("OpenAI images response contained an invalid data URL".into())
        );
    }

    #[tokio::test]
    async fn datums_without_image_data_are_rejected() {
        assert_eq!(
            resolve_image(&json!({}), "png", None).await,
            Err("OpenAI images response datum contained no image data".into())
        );
        assert_eq!(
            resolve_image(&json!({ "url": "ftp://example.com/x.png" }), "png", None).await,
            Err("OpenAI images response URL must use HTTP or HTTPS".into())
        );
    }
}
