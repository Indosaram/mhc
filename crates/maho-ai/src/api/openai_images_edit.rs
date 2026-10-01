//! Port of senpi packages/ai/src/api/openai-images-edit.ts.

use serde_json::{Map, Value};

use crate::types::ImageContent;

pub fn upload_name(image: &ImageContent, name: &str) -> String {
    let extension = image.mime_type.split('/').nth(1).unwrap_or("bin");
    format!("{name}.{extension}")
}

pub fn upload_bytes(image: &ImageContent) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(&image.data)
        .map_err(|error| error.to_string())
}

pub fn build_edit_params(
    params: &Map<String, Value>,
    images: &[ImageContent],
    mask: Option<&ImageContent>,
) -> Result<Map<String, Value>, String> {
    if images.len() > 16 {
        return Err("[OI] image edits accept at most 16 reference images".into());
    }
    let mut params = params.clone();
    params.insert("image".into(), Value::Array(images.iter().map(|image| serde_json::to_value(image).unwrap_or(Value::Null)).collect()));
    if let Some(mask) = mask {
        params.insert("mask".into(), serde_json::to_value(mask).unwrap_or(Value::Null));
    }
    Ok(params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn image(mime_type: &str) -> ImageContent {
        ImageContent { data: "AA==".into(), mime_type: mime_type.to_owned() }
    }

    #[test]
    fn upload_names_carry_the_mime_extension() {
        assert_eq!(upload_name(&image("image/png"), "reference-0"), "reference-0.png");
        assert_eq!(upload_name(&image("image/jpeg"), "mask"), "mask.jpeg");
    }

    #[test]
    fn edit_params_attach_images_and_an_optional_mask() {
        let params: Map<String, Value> = serde_json::from_value(json!({ "model": "m", "prompt": "p" })).expect("params");
        let references = vec![image("image/png"), image("image/webp")];
        let without_mask = build_edit_params(&params, &references, None).expect("edit params");
        assert_eq!(without_mask["image"].as_array().map(Vec::len), Some(2));
        assert!(without_mask.get("mask").is_none());

        let with_mask = build_edit_params(&params, &references, Some(&image("image/png"))).expect("edit params");
        assert_eq!(with_mask["mask"]["mimeType"], json!("image/png"));
        assert_eq!(with_mask["model"], json!("m"));
    }

    #[test]
    fn more_than_sixteen_references_are_rejected() {
        let params: Map<String, Value> = Map::new();
        let references: Vec<ImageContent> = (0..17).map(|_| image("image/png")).collect();
        assert_eq!(
            build_edit_params(&params, &references, None),
            Err("[OI] image edits accept at most 16 reference images".into())
        );
    }
}
