use maho_ext_imagegen::params::{parameters, DEFAULT_IMAGE_MODEL};
use serde_json::json;

#[test]
fn schema_keeps_source_defaults_required_fields_and_limits() {
    let schema = parameters();
    assert_eq!(schema["required"], json!(["prompt"]));
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"]["model"]["default"], DEFAULT_IMAGE_MODEL);
    assert_eq!(schema["properties"]["prompt"]["maxLength"], 32000);
    assert_eq!(schema["properties"]["n"]["maximum"], 10);
    assert_eq!(schema["properties"]["output_compression"]["maximum"], 100);
    assert_eq!(schema["properties"]["reference_image_paths"]["maxItems"], 5);
}
