use base64::Engine;
#[derive(Default)]
pub struct ProcessImageOptions { pub auto_resize_images: Option<bool>, pub resize_options: Option<super::image_resize::ImageResizeOptions> }
pub struct ProcessedImage { pub data: String, pub mime_type: String, pub hints: Vec<String> }
pub async fn process_image(bytes: &[u8], mime_type: &str, options: ProcessImageOptions) -> Result<ProcessedImage, String> {
    let base = mime_type.split(';').next().unwrap_or(mime_type).trim().to_lowercase();
    let supported = match base.as_str() { "image/png" => Some("image/png"), "image/jpeg" | "image/jpg" => Some("image/jpeg"), "image/gif" => Some("image/gif"), "image/webp" => Some("image/webp"), _ => None };
    let (bytes, mime, converted) = if let Some(mime) = supported { (bytes.to_vec(), mime, None) } else { let bytes = super::image_convert::convert_image_bytes_to_png(bytes).ok_or("[Image omitted: could not be converted to a supported inline image format.]")?; (bytes, "image/png", Some(base)) };
    if options.auto_resize_images.unwrap_or(true) {
        let result = super::image_resize::resize_image(&bytes, mime, options.resize_options.unwrap_or_default()).await.ok_or("[Image omitted: could not be resized below the inline image size limit.]")?;
        let mut hints = Vec::new(); if let Some(from) = converted.filter(|from| !from.is_empty() && from != &result.mime_type) { hints.push(format!("[Image converted from {from} to {}.]", result.mime_type)); }
        if let Some(note) = super::image_resize::format_dimension_note(&result) { hints.push(note); }
        Ok(ProcessedImage { data: result.data, mime_type: result.mime_type, hints })
    } else { let hints = converted.filter(|from| !from.is_empty() && from != mime).map(|from| vec![format!("[Image converted from {from} to {mime}.]")]).unwrap_or_default(); Ok(ProcessedImage { data: base64::engine::general_purpose::STANDARD.encode(bytes), mime_type: mime.to_owned(), hints }) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn partial_model_profile_reaches_native_resize() {
        // Given a height-only model profile and an in-memory landscape PNG.
        let bytes = photon_rs::PhotonImage::new(vec![255; 16 * 8 * 4], 16, 8).get_bytes();
        let profile = maho_ai::model::ModelImageResizeOptions {
            max_height: Some(4), ..Default::default()
        };
        // When the shared model profile is converted at the consumer boundary.
        let result = process_image(&bytes, "image/png", ProcessImageOptions {
            resize_options: Some((&profile).into()), ..Default::default()
        }).await.expect("processed PNG");
        // Then the native result respects the supplied axis and aspect ratio.
        let decoded = photon_rs::PhotonImage::new_from_byteslice(
            base64::engine::general_purpose::STANDARD.decode(&result.data).expect("base64")
        );
        assert_eq!((decoded.get_width(), decoded.get_height()), (8, 4));
    }
}
