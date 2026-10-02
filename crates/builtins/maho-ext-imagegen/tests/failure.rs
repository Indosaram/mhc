use maho_ext_imagegen::params::*;
#[test]
fn failure_retains_request_metadata_and_reports_no_outputs() {
    let result = failure("failed", FailureReason::WriteFailed, GenerateImageBase { model: DEFAULT_IMAGE_MODEL, size: "auto", quality: "high", background: "opaque", output_format: "png", requested: 3, source: "provider" });
    assert_eq!(result["details"]["requested"], 3);
    assert_eq!(result["details"]["generated"], 0);
    assert_eq!(result["details"]["reason"], "write_failed");
    assert_eq!(result["details"]["paths"], serde_json::json!([]));
    assert_eq!(result["content"][0]["text"], result["details"]["error"]);
}
