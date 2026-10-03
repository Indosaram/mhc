use std::{future::Future, pin::Pin};
use base64::Engine;
use serde::{Serialize, Deserialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalImageContent { pub data: String, pub mime_type: String }

pub struct EvalImageResizeResult { pub image: EvalImageContent, pub dimension_note: Option<String> }
pub struct ResizedImage { pub data: String, pub mime_type: String, pub dimension_note: String }
pub type ImageFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

pub trait EvalImageSdk: Send + Sync {
    fn resize_image<'a>(&'a self, bytes: Vec<u8>, mime_type: &'a str, max_bytes: Option<usize>) -> ImageFuture<'a, Option<ResizedImage>>;
    fn convert_to_png<'a>(&'a self, data: &'a str, mime_type: &'a str) -> ImageFuture<'a, Option<EvalImageContent>>;
}

pub fn webp_exclusion_for_model(provider: Option<&str>, api: Option<&str>) -> bool {
    matches!(provider, Some("ollama" | "ollama-cloud" | "llama.cpp" | "lm-studio" | "local-server")) || api == Some("ollama-chat")
}

pub async fn resize_eval_image(image: EvalImageContent, provider: Option<&str>, api: Option<&str>, sdk: &dyn EvalImageSdk) -> Result<EvalImageResizeResult, String> {
    let exclude_webp = webp_exclusion_for_model(provider, api);
    let max_bytes = (exclude_webp && image.mime_type == "image/webp").then_some(image.data.len());
    let bytes = base64::engine::general_purpose::STANDARD.decode(&image.data).map_err(|error|error.to_string())?;
    let resized = sdk.resize_image(bytes, &image.mime_type, max_bytes).await?;
    let (mut output, dimension_note) = match resized {
        Some(resized) => (EvalImageContent {data:resized.data,mime_type:resized.mime_type}, Some(resized.dimension_note)),
        None => (image, None),
    };
    if exclude_webp && output.mime_type == "image/webp" {
        output = sdk.convert_to_png(&output.data, &output.mime_type).await?.ok_or_else(||format!("Unable to convert {} display output for the active model", output.mime_type))?;
    }
    Ok(EvalImageResizeResult {image:output,dimension_note})
}
