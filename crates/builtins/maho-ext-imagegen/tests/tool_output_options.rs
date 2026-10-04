#[path = "support.rs"]
mod support;

use maho_ai::types::ImagesBackground;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn defaults_to_sunburst_and_prices_from_the_static_catalog() {
    let root = tempfile::tempdir().expect("temp");
    let stub = Arc::new(support::StubImages::one());
    let result = support::run(root.path(), true, stub.clone(), "cost", json!({"prompt":"a red fox"})).await;

    let (model, _, _) = stub.first_call().expect("call");
    assert_eq!(model.id, "gpt-image-2.5-sunburst");
    assert_eq!((model.cost.input, model.cost.output, model.cost.cache_read, model.cost.cache_write), (5.0, 30.0, 1.25, 0.0));
    assert_eq!(result.details["model"], "gpt-image-2.5-sunburst");
}

#[tokio::test]
async fn forwards_background_format_compression_and_moderation() {
    let root = tempfile::tempdir().expect("temp");
    let stub = Arc::new(support::StubImages::one());
    let result = support::run(root.path(), true, stub.clone(), "opts", json!({"prompt":"a red fox","background":"opaque","output_format":"webp","output_compression":60,"moderation":"low","output_path":"art/fox.webp"})).await;

    let (_, _, options) = stub.first_call().expect("call");
    assert_eq!(options.extra["background"], json!("opaque"));
    assert_eq!(options.extra["outputFormat"], json!("webp"));
    assert_eq!(options.extra["outputCompression"], json!(60));
    assert_eq!(options.extra["moderation"], json!("low"));
    assert_eq!(result.details["background"], "opaque");
    assert_eq!(result.details["outputFormat"], "webp");
    assert_eq!(result.details["paths"], json!(["art/fox.webp"]));
    assert!(root.path().join("art/fox.webp").exists());
}

#[tokio::test]
async fn keeps_png_defaults_on_the_wire_and_in_details() {
    let root = tempfile::tempdir().expect("temp");
    let stub = Arc::new(support::StubImages::one());
    let result = support::run(root.path(), true, stub.clone(), "defaults", json!({"prompt":"a red fox","output_path":"fox"})).await;

    let (_, _, options) = stub.first_call().expect("call");
    assert_eq!(options.extra["outputFormat"], json!("png"));
    assert!(!options.extra.contains_key("background"));
    assert!(!options.extra.contains_key("outputCompression"));
    assert!(!options.extra.contains_key("moderation"));
    assert_eq!(result.details["outputFormat"], "png");
    assert_eq!(result.details["background"], "auto");
    assert_eq!(result.details["paths"], json!(["fox.png"]));
    assert!(result.details.get("transparentBackground").is_none());
}

#[tokio::test]
async fn derives_the_extension_for_the_requested_format() {
    for (output_format, output_path, expected) in [("jpeg", "art/fox", "art/fox.jpg"), ("jpeg", "art/fox.jpeg", "art/fox.jpeg"), ("jpeg", "art/fox.JPG", "art/fox.JPG"), ("webp", "art/fox", "art/fox.webp")] {
        let root = tempfile::tempdir().expect("temp");
        let result = support::run(root.path(), true, Arc::new(support::StubImages::one()), "format", json!({"prompt":"a red fox","output_format":output_format,"output_path":output_path})).await;
        assert_eq!(result.details["paths"], json!([expected]), "{output_format} {output_path}");
        assert!(root.path().join(expected).exists());
    }
}

#[tokio::test]
async fn names_an_omitted_output_path_after_the_format() {
    let root = tempfile::tempdir().expect("temp");
    let stub = Arc::new(support::StubImages { images: 2, ..Default::default() });
    support::run(root.path(), true, stub, "named", json!({"prompt":"a red fox","output_format":"webp","n":2})).await;

    let mut entries = std::fs::read_dir(root.path().join("generated-images")).expect("dir").map(|entry| entry.expect("entry").file_name().into_string().expect("name")).collect::<Vec<_>>();
    entries.sort();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|entry| entry.ends_with("-01.webp") || entry.ends_with("-02.webp")));
}

#[tokio::test]
async fn rejects_an_output_path_that_mismatches_the_format() {
    for (output_path, output_format, expected_extension) in [("fox.png", "webp", ".webp"), ("fox.jpg", "png", ".png"), ("fox.webp", "jpeg", ".jpg")] {
        let root = tempfile::tempdir().expect("temp");
        let stub = Arc::new(support::StubImages::one());
        let result = support::run(root.path(), true, stub.clone(), "mismatch", json!({"prompt":"a red fox","output_path":output_path,"output_format":output_format})).await;
        assert_eq!(result.details["reason"], "invalid_params");
        assert!(result.details["error"].as_str().expect("error").contains(expected_extension));
        assert_eq!(stub.call_count(), 0);
    }
}

#[tokio::test]
async fn rejects_invalid_output_option_combinations_before_any_request() {
    for (params, needle) in [
        (json!({"background":"transparent","output_format":"jpeg"}), "requires output_format png or webp"),
        (json!({"output_compression":50}), "requires output_format jpeg or webp"),
        (json!({"output_compression":50,"output_format":"png"}), "requires output_format jpeg or webp"),
    ] {
        let root = tempfile::tempdir().expect("temp");
        let stub = Arc::new(support::StubImages::one());
        let mut body = json!({"prompt":"a red fox"});
        for (key, value) in params.as_object().expect("object") {
            body[key] = value.clone();
        }
        let result = support::run(root.path(), true, stub.clone(), "invalid", body).await;
        assert_eq!(result.details["reason"], "invalid_params");
        assert!(result.details["error"].as_str().expect("error").contains(needle), "{needle}");
        assert_eq!(stub.call_count(), 0);
    }
}

#[tokio::test]
async fn sends_mask_image_path_and_requires_a_reference() {
    let root = tempfile::tempdir().expect("temp");
    std::fs::write(root.path().join("reference.png"), support::PNG_BASE64).expect("reference");
    std::fs::write(root.path().join("mask.png"), support::PNG_BASE64).expect("mask");
    let stub = Arc::new(support::StubImages::one());
    let result = support::run(root.path(), true, stub.clone(), "mask", json!({"prompt":"replace the sky","reference_image_paths":["reference.png"],"mask_image_path":"mask.png"})).await;

    assert_eq!(result.details["generated"], 1);
    let (_, context, options) = stub.first_call().expect("call");
    assert_eq!(options.extra["mask"]["data"], support::PNG_BASE64);
    assert_eq!(options.extra["mask"]["mimeType"], "image/png");
    assert_eq!(context.input.len(), 2);

    let stub2 = Arc::new(support::StubImages::one());
    let rejected = support::run(root.path(), true, stub2.clone(), "no-ref", json!({"prompt":"replace the sky","mask_image_path":"mask.png"})).await;
    assert_eq!(rejected.details["reason"], "invalid_params");
    assert!(rejected.details["error"].as_str().expect("error").contains("mask"));
    assert_eq!(stub2.call_count(), 0);
}

#[tokio::test]
async fn rejects_a_mask_that_is_not_a_readable_image() {
    let root = tempfile::tempdir().expect("temp");
    std::fs::write(root.path().join("reference.png"), support::PNG_BASE64).expect("reference");
    std::fs::write(root.path().join("mask.png"), "not a png").expect("mask");
    let stub = Arc::new(support::StubImages::one());
    let result = support::run(root.path(), true, stub.clone(), "bad-mask", json!({"prompt":"x","reference_image_paths":["reference.png"],"mask_image_path":"mask.png"})).await;

    assert_eq!(result.details["reason"], "invalid_params");
    assert!(result.details["error"].as_str().expect("error").contains("mask.png"));
    assert_eq!(stub.call_count(), 0);
}

#[tokio::test]
async fn renames_the_file_when_the_provider_returns_a_different_format() {
    let root = tempfile::tempdir().expect("temp");
    let stub = Arc::new(support::StubImages { images: 1, mime_type: Some("image/png".into()), ..Default::default() });
    let result = support::run(root.path(), true, stub, "rename", json!({"prompt":"a red fox","output_format":"webp","output_path":"art/fox.webp"})).await;

    assert_eq!(result.details["paths"], json!(["art/fox.png"]));
    assert_eq!(result.details["outputFormat"], "png");
    assert!(root.path().join("art/fox.png").exists());
    assert!(!root.path().join("art/fox.webp").exists());
    assert!(support::text_of(&result).contains("returned png instead of the requested webp"));
}

#[tokio::test]
async fn names_the_colliding_renamed_target_and_writes_nothing() {
    let root = tempfile::tempdir().expect("temp");
    std::fs::write(root.path().join("fox-02.png"), "occupied").expect("occupied");
    let stub = Arc::new(support::StubImages { images: 2, mime_type: Some("image/png".into()), ..Default::default() });
    let result = support::run(root.path(), true, stub, "collide", json!({"prompt":"a red fox","output_format":"webp","output_path":"fox.webp","n":2})).await;

    assert_eq!(result.details["reason"], "write_failed");
    assert!(result.details["error"].as_str().expect("error").contains("fox-02.png"));
    assert!(!root.path().join("fox-01.png").exists());
    assert!(!root.path().join("fox-01.webp").exists());
}

#[tokio::test]
async fn reports_the_providers_transparency_verdict() {
    let root = tempfile::tempdir().expect("temp");
    let transparent = Arc::new(support::StubImages { images: 1, background: Some(ImagesBackground::Transparent), ..Default::default() });
    let result = support::run(root.path(), true, transparent, "transparent", json!({"prompt":"a sticker","background":"transparent","output_format":"webp"})).await;
    assert_eq!(result.details["background"], "transparent");
    assert_eq!(result.details["transparentBackground"], true);

    let opaque = Arc::new(support::StubImages { images: 1, background: Some(ImagesBackground::Opaque), ..Default::default() });
    let result = support::run(root.path(), true, opaque, "opaque", json!({"prompt":"a sticker","background":"auto"})).await;
    assert_eq!(result.details["transparentBackground"], false);
}
