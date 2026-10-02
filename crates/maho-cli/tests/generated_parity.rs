use maho_cli::utils::{git::parse_git_url, image_convert::convert_to_png, image_resize_core::{resize_image_in_process, ImageResizeOptions}};
use serde_json::{json, Value};

#[test]
fn hosted_git_generated_reference_cases() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("golden/git-38.json")).unwrap();
    for case in cases {
        let input = case["input"].as_str().unwrap();
        let parsed = parse_git_url(input).unwrap();
        let mut actual = json!({"type":"git", "repo":parsed.repo, "host":parsed.host, "path":parsed.path, "pinned":parsed.pinned});
        if let Some(reference) = parsed.reference { actual["ref"] = reference.into(); }
        assert_eq!(actual, case["result"], "{input}");
    }
}

#[test]
fn image_generated_reference_cases() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("golden/images-38.json")).unwrap();
    for case in cases {
        let input = case["input"].as_str().unwrap();
        let mime = case["mime"].as_str().unwrap();
        let bytes = maho_cli::utils::image_convert::decode_base64(input);
        let options = ImageResizeOptions { max_width: case["options"]["maxWidth"].as_u64().unwrap() as u32, max_height: case["options"]["maxHeight"].as_u64().unwrap() as u32, max_bytes: case["options"]["maxBytes"].as_f64().unwrap(), ..Default::default() };
        let actual = resize_image_in_process(&bytes, mime, &options).map(|image| json!({"data":image.data,"mimeType":image.mime_type,"originalWidth":image.original_width,"originalHeight":image.original_height,"width":image.width,"height":image.height,"wasResized":image.was_resized}));
        assert_eq!(actual.unwrap_or(Value::Null), case["resized"], "resize {}", case["name"]);
        let converted = convert_to_png(input, mime).map(|image| json!({"data":image.data,"mimeType":image.mime_type}));
        assert_eq!(converted.unwrap_or(Value::Null), case["converted"], "convert {}", case["name"]);
    }
}
