#[path = "support.rs"]
mod support;

use maho_ai::types::{Usage, UsageCost};
use maho_ext_imagegen::state::set_native_bypass;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn saves_image_reports_path_and_records_revised_prompts() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    let stub = Arc::new(support::StubImages {
        images: 1,
        revised_prompts: std::sync::Mutex::new(vec!["A richly detailed red fox".into()]),
        usage: Some(Usage { input: 12, output: 34, cache_read: 0, cache_write: 0, total_tokens: 46, cost: UsageCost { input: 0.1, output: 0.2, cache_read: 0.0, cache_write: 0.0, total: 0.3 }, ..Default::default() }),
        ..Default::default()
    });
    let result = support::run(root.path(), true, stub, "call-1", json!({"prompt":"a red fox","output_path":"art/fox.png"})).await;

    let absolute = root.path().join("art/fox.png");
    assert!(absolute.exists());
    assert_eq!(std::fs::read(&absolute).expect("bytes")[..4], [0x89, 0x50, 0x4e, 0x47]);
    assert_eq!(result.details["paths"], json!(["art/fox.png"]));
    assert_eq!(result.details["revisedPrompts"], json!(["A richly detailed red fox"]));
    assert_eq!(result.details["model"], "gpt-image-2.5-sunburst");
    assert_eq!(result.details["requested"], 1);
    assert_eq!(result.details["generated"], 1);
    assert_eq!(result.details["size"], "auto");
    assert_eq!(result.details["quality"], "auto");
    assert!(support::text_of(&result).contains("art/fox.png"));
    assert_eq!(support::image_count(&result), 1);
    assert_eq!(result.usage.expect("usage").total_tokens, 46);
}

#[tokio::test]
async fn omitted_output_path_defaults_to_a_sanitized_generated_images_file() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    let result = support::run(root.path(), true, Arc::new(support::StubImages::one()), "safe-id", json!({"prompt":"a blue whale"})).await;

    let entries = std::fs::read_dir(root.path().join("generated-images")).expect("dir").map(|entry| entry.expect("entry").file_name().into_string().expect("name")).collect::<Vec<_>>();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].chars().all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '-' || character == '.'));
    assert!(entries[0].ends_with(".png"));
    assert_eq!(result.details["paths"], json!([format!("generated-images/{}", entries[0])]));
}

#[tokio::test]
async fn no_credentials_yields_structured_missing_config_without_calling_the_provider() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    let stub = Arc::new(support::StubImages::one());
    let registry = Arc::new(support::FixtureRegistry { stored_api_key: false, provider_api_key: None, provider_headers: None, models: Vec::new() });
    let result = support::run_with(root.path(), registry, stub.clone(), "missing", json!({"prompt":"a red fox"})).await;

    assert_eq!(result.details["reason"], "missing_config");
    let text = support::text_of(&result);
    assert!(text.contains("openai"));
    assert!(text.contains("PI_IMAGE_GEN_PROVIDER"));
    assert!(text.contains("OPENAI_API_KEY"));
    assert_eq!(stub.call_count(), 0);
}

#[tokio::test]
async fn gateway_base_url_provider_and_key_thread_through_options() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    let stub = Arc::new(support::StubImages::one());
    let result = support::run(root.path(), true, stub.clone(), "gw", json!({"prompt":"a red fox","size":"1024x1536","quality":"high"})).await;

    let (model, _context, options) = stub.first_call().expect("call");
    assert_eq!(model.base_url, "https://gateway.example/openai/v1");
    assert_eq!(model.provider, "quotio-openai");
    assert_eq!(model.api, "openai-images");
    assert_eq!(model.id, "gpt-image-2.5-sunburst");
    assert_eq!(options.request.api_key.as_deref(), Some("gateway-secret"));
    assert_eq!(options.extra["size"], json!("1024x1536"));
    assert_eq!(options.extra["quality"], json!("high"));
    assert_eq!(options.extra["n"], json!(1));
    assert_eq!(result.details["source"], "provider-config:quotio-openai");
    assert!(!serde_json::to_string(&result.details).expect("json").contains("gateway-secret"));
}

#[tokio::test]
async fn blank_prompt_and_wrong_extension_are_rejected_before_the_provider() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    let stub = Arc::new(support::StubImages::one());
    let blank = support::run(root.path(), true, stub.clone(), "blank", json!({"prompt":"   "})).await;
    let wrong = support::run(root.path(), true, stub.clone(), "wrong", json!({"prompt":"a red fox","output_path":"art/fox.jpg"})).await;

    assert!(support::text_of(&blank).contains("prompt"));
    assert!(support::text_of(&wrong).contains(".png"));
    assert_eq!(stub.call_count(), 0);
}

#[tokio::test]
async fn extensionless_output_path_gains_a_png_extension() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    let result = support::run(root.path(), true, Arc::new(support::StubImages::one()), "ext", json!({"prompt":"a red fox","output_path":"art/fox"})).await;

    assert_eq!(result.details["paths"], json!(["art/fox.png"]));
    assert!(root.path().join("art/fox.png").exists());
}

#[tokio::test]
async fn multiple_images_are_indexed_before_the_extension() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    let stub = Arc::new(support::StubImages { images: 2, ..Default::default() });
    let result = support::run(root.path(), true, stub.clone(), "multi", json!({"prompt":"a red fox","output_path":"art/fox.png","n":2})).await;

    assert_eq!(result.details["paths"], json!(["art/fox-01.png", "art/fox-02.png"]));
    assert!(root.path().join("art/fox-01.png").exists());
    assert!(root.path().join("art/fox-02.png").exists());
    assert_eq!(support::image_count(&result), 2);
    assert_eq!(stub.first_call().expect("call").2.extra["n"], json!(2));
}

#[tokio::test]
async fn existing_file_is_never_overwritten() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    std::fs::create_dir_all(root.path().join("art")).expect("dir");
    std::fs::write(root.path().join("art/fox.png"), "original").expect("existing");
    let stub = Arc::new(support::StubImages::one());
    let result = support::run(root.path(), true, stub.clone(), "existing", json!({"prompt":"a red fox","output_path":"art/fox.png"})).await;

    assert!(support::text_of(&result).contains("art/fox.png"));
    assert_eq!(std::fs::read_to_string(root.path().join("art/fox.png")).expect("read"), "original");
    assert_eq!(stub.call_count(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn a_later_write_failure_removes_this_invocations_files() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    std::fs::create_dir_all(root.path().join("art")).expect("dir");
    std::os::unix::fs::symlink(root.path().join("art/missing-target"), root.path().join("art/fox-02.png")).expect("symlink");
    let stub = Arc::new(support::StubImages { images: 2, ..Default::default() });
    let result = support::run(root.path(), true, stub, "rollback", json!({"prompt":"a red fox","output_path":"art/fox.png","n":2})).await;

    assert!(!root.path().join("art/fox-01.png").exists());
    assert!(support::text_of(&result).to_lowercase().contains("failed"));
    assert_eq!(result.details["paths"], json!([]));
}

#[tokio::test]
async fn native_bypass_defers_without_calling_the_provider() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    set_native_bypass(true);
    let stub = Arc::new(support::StubImages::one());
    let result = support::run(root.path(), true, stub.clone(), "bypass", json!({"prompt":"a red fox"})).await;
    set_native_bypass(false);

    assert_eq!(result.details["reason"], "provider_native_bypass");
    assert_eq!(stub.call_count(), 0);
}

#[tokio::test]
async fn provider_failure_is_reported_without_writing_files() {
    let _guard = support::GlobalStateGuard::acquire(None).await;
    let root = tempfile::tempdir().expect("temp");
    let stub = Arc::new(support::StubImages { images: 1, error: Some("upstream refused the request".into()), ..Default::default() });
    let result = support::run(root.path(), true, stub, "failure", json!({"prompt":"a red fox","output_path":"art/fox.png"})).await;

    assert!(support::text_of(&result).contains("upstream refused the request"));
    assert!(!root.path().join("art/fox.png").exists());
    assert_eq!(result.details["paths"], json!([]));
}
