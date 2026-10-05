#[path = "support.rs"]
pub mod support;

use maho_ai::types::{ContentBlock, InputModality};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn defaults_to_sunburst_and_saves_the_returned_png() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    let stub = Arc::new(support::StubImages::one());
    let result = support::run(root.path(), false, stub.clone(), "sunburst", json!({"prompt":"a red fox","output_path":"fox.png"})).await;

    let (model, _, _) = stub.first_call().expect("call");
    assert_eq!(model.id, "gpt-image-2.5-sunburst");
    assert_eq!(model.name, "GPT Image 2.5 Sunburst");
    assert_eq!(model.input, vec![InputModality::Text, InputModality::Image]);
    assert_eq!(result.details["model"], "gpt-image-2.5-sunburst");
    assert_eq!(std::fs::read(root.path().join("fox.png")).expect("bytes"), base64_decode(support::PNG_BASE64));
}

#[tokio::test]
async fn passes_the_selected_model_quality_and_arbitrary_size() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    for quality in ["max", "xhigh"] {
        let root = tempfile::tempdir().expect("temp");
        let stub = Arc::new(support::StubImages::one());
        let result = support::run(root.path(), false, stub.clone(), "quality", json!({"prompt":"a red fox","model":"gpt-image-2.5-flare","quality":quality,"size":"2048x1152"})).await;
        let (model, _, options) = stub.first_call().expect("call");
        assert_eq!(model.id, "gpt-image-2.5-flare");
        assert_eq!(model.name, "GPT Image 2.5 Flare");
        assert_eq!(options.extra["quality"], json!(quality));
        assert_eq!(options.extra["size"], json!("2048x1152"));
        assert_eq!(result.details["model"], "gpt-image-2.5-flare");
        assert_eq!(result.details["quality"], quality);
        assert_eq!(result.details["size"], "2048x1152");
    }
}

#[tokio::test]
async fn keeps_the_legacy_model_selectable() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    let stub = Arc::new(support::StubImages::one());
    let result = support::run(root.path(), false, stub.clone(), "legacy", json!({"prompt":"a red fox","model":"gpt-image-2","quality":"max"})).await;

    let (model, _, options) = stub.first_call().expect("call");
    assert_eq!(model.id, "gpt-image-2");
    assert_eq!(model.name, "GPT Image 2");
    assert_eq!(options.extra["quality"], json!("max"));
    assert_eq!(result.details["model"], "gpt-image-2");
}

#[tokio::test]
async fn rejects_invalid_sizes_without_calling_the_provider() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    for size in ["1000x1000", "4096x2048", "512x512", "3840x3840", "3072x768"] {
        let root = tempfile::tempdir().expect("temp");
        let stub = Arc::new(support::StubImages::one());
        let result = support::run(root.path(), false, stub.clone(), "bad-size", json!({"prompt":"a red fox","model":"gpt-image-2.5-flare","size":size})).await;
        assert_eq!(result.details["reason"], "invalid_params", "{size}");
        assert_eq!(result.details["model"], "gpt-image-2.5-flare");
        assert_eq!(stub.call_count(), 0);
    }
}

#[tokio::test]
async fn accepts_popular_sizes() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    for size in ["2048x2048", "3840x2160", "2160x3840"] {
        let root = tempfile::tempdir().expect("temp");
        let stub = Arc::new(support::StubImages::one());
        support::run(root.path(), false, stub.clone(), "size", json!({"prompt":"a red fox","size":size})).await;
        assert_eq!(stub.first_call().expect("call").2.extra["size"], json!(size));
    }
}

#[tokio::test]
async fn sends_a_local_png_after_the_text_input() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    for absolute in [false, true] {
        let root = tempfile::tempdir().expect("temp");
        let path = root.path().join("reference.png");
        std::fs::write(&path, base64_decode(support::PNG_BASE64)).expect("reference");
        let reference = if absolute { path.to_string_lossy().into_owned() } else { "reference.png".into() };
        let stub = Arc::new(support::StubImages::one());
        let result = support::run(root.path(), false, stub.clone(), "reference", json!({"prompt":"  the same fox wearing a blue scarf  ","reference_image_paths":[reference]})).await;

        assert_eq!(result.details["generated"], 1);
        let (_, context, _) = stub.first_call().expect("call");
        assert_eq!(context.input.len(), 2);
        match &context.input[0] {
            ContentBlock::Text(text) => assert_eq!(text.text, "the same fox wearing a blue scarf"),
            other => panic!("expected text, got {other:?}"),
        }
        match &context.input[1] {
            ContentBlock::Image(image) => {
                assert_eq!(image.data, support::PNG_BASE64);
                assert_eq!(image.mime_type, "image/png");
            }
            other => panic!("expected image, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn recognizes_jpeg_and_webp_magic_bytes_rather_than_extensions() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    let jpeg = [0xff, 0xd8, 0xff, 0xe0];
    let webp = hex("524946460400000057454250");
    std::fs::write(root.path().join("first.dat"), jpeg).expect("jpeg");
    std::fs::write(root.path().join("second.dat"), &webp).expect("webp");
    let stub = Arc::new(support::StubImages::one());
    support::run(root.path(), false, stub.clone(), "magic", json!({"prompt":"combine these references","reference_image_paths":["first.dat","second.dat"]})).await;

    let (_, context, _) = stub.first_call().expect("call");
    assert_eq!(context.input.len(), 3);
    match &context.input[1] {
        ContentBlock::Image(image) => {
            assert_eq!(image.data, base64_encode(&jpeg));
            assert_eq!(image.mime_type, "image/jpeg");
        }
        other => panic!("expected jpeg, got {other:?}"),
    }
    match &context.input[2] {
        ContentBlock::Image(image) => {
            assert_eq!(image.data, base64_encode(&webp));
            assert_eq!(image.mime_type, "image/webp");
        }
        other => panic!("expected webp, got {other:?}"),
    }
}

#[tokio::test]
async fn rejects_reference_counts_outside_one_to_five() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    for count in [0usize, 6] {
        let root = tempfile::tempdir().expect("temp");
        let stub = Arc::new(support::StubImages::one());
        let references = (0..count).map(|index| format!("{index}.png")).collect::<Vec<_>>();
        let result = support::run(root.path(), false, stub.clone(), "count", json!({"prompt":"a red fox","reference_image_paths":references})).await;
        assert_eq!(result.details["reason"], "invalid_params", "count {count}");
        assert_eq!(stub.call_count(), 0);
    }
}

#[tokio::test]
async fn accepts_five_references_in_order() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    std::fs::write(root.path().join("reference.png"), base64_decode(support::PNG_BASE64)).expect("reference");
    let stub = Arc::new(support::StubImages::one());
    support::run(root.path(), false, stub.clone(), "five", json!({"prompt":"a red fox","reference_image_paths":["reference.png","reference.png","reference.png","reference.png","reference.png"]})).await;

    assert_eq!(stub.first_call().expect("call").1.input.len(), 6);
}

#[tokio::test]
async fn rejects_invalid_references_and_names_them() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    std::fs::write(root.path().join("not-an-image.png"), "plain text, not a PNG").expect("text");
    std::fs::write(root.path().join("fake.webp"), hex("d2c9c6c604000000d7c5c2d0")).expect("fake");
    let large = root.path().join("large.png");
    std::fs::File::create(&large).expect("create").set_len(50 * 1024 * 1024 + 1).expect("truncate");

    for path in ["missing.png", ".", "not-an-image.png", "fake.webp", "large.png"] {
        let stub = Arc::new(support::StubImages::one());
        let result = support::run(root.path(), false, stub.clone(), "invalid-ref", json!({"prompt":"a red fox","reference_image_paths":[path]})).await;
        assert_eq!(result.details["reason"], "invalid_params", "{path}");
        assert!(result.details["error"].as_str().expect("error").contains(path), "{path}");
        assert_eq!(stub.call_count(), 0);
    }
}

fn hex(value: &str) -> Vec<u8> {
    (0..value.len()).step_by(2).map(|index| u8::from_str_radix(&value[index..index + 2], 16).expect("hex")).collect()
}

fn base64_decode(value: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(value).expect("base64")
}

fn base64_encode(value: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(value)
}
