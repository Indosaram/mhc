pub use super::image_resize_core::{ImageResizeOptions, ResizedImage};
pub async fn resize_image(bytes: &[u8], mime: &str, options: ImageResizeOptions) -> Option<ResizedImage> {
    let bytes = bytes.to_vec();
    let mime = mime.to_owned();
    tokio::task::spawn_blocking(move || super::image_resize_core::resize_image_in_process(&bytes, &mime, &options)).await.ok().flatten()
}
pub fn format_dimension_note(result: &ResizedImage) -> Option<String> {
    result.was_resized.then(|| format!("[Image: original {}x{}, displayed at {}x{}. Multiply coordinates by {:.2} to map to original image.]", result.original_width, result.original_height, result.width, result.height, result.original_width as f64 / result.width as f64))
}
