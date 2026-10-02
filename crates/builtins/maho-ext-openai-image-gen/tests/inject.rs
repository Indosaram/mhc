use maho_ext_openai_image_gen::inject::{apply_image_generation_tools, ImageGenMode};
use maho_ext_imagegen::params::DEFAULT_IMAGE_MODEL;
use serde_json::json;

#[test]
fn native_injects_pinned_tool_and_removes_client() {
    let payload = json!({"tools":[{"type":"function","name":"generate_image"},{"type":"function","name":"read"}]});
    let result = apply_image_generation_tools(&payload, ImageGenMode::Native);
    assert_eq!(result["tools"], json!([{"type":"function","name":"read"},{"type":"image_generation","model":DEFAULT_IMAGE_MODEL}]));
}
#[test]
fn native_replaces_duplicate_and_dated_entries() {
    let payload = json!({"tools":[{"type":"image_generation"},{"type":"function","name":"read"},{"type":"image_generation_2025","model":"gpt-image-1"}]});
    assert_eq!(apply_image_generation_tools(&payload, ImageGenMode::Native)["tools"], json!([{"type":"function","name":"read"},{"type":"image_generation","model":DEFAULT_IMAGE_MODEL}]));
}
#[test]
fn client_and_unavailable_strip_their_excluded_surfaces() {
    let payload = json!({"tools":[{"type":"image_generation"},{"name":"generate_image"},{"name":"read"}]});
    assert_eq!(apply_image_generation_tools(&payload, ImageGenMode::Client)["tools"], json!([{"name":"generate_image"},{"name":"read"}]));
    assert_eq!(apply_image_generation_tools(&payload, ImageGenMode::Unavailable)["tools"], json!([{"name":"read"}]));
}
#[test]
fn unchanged_client_payload_preserves_all_values() {
    let payload = json!({"tools":[{"name":"generate_image"},{"name":"read"}],"other":3});
    assert_eq!(apply_image_generation_tools(&payload, ImageGenMode::Client), payload);
}
