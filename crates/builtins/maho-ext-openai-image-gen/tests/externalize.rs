use maho_ai::types::AssistantMessage;
use maho_ext_openai_image_gen::externalize::*;
use serde_json::json;

fn message(result: &str) -> AssistantMessage {
    serde_json::from_value(json!({"role":"assistant","api":"openai-responses","provider":"openai","model":"model","timestamp":0,"stopReason":"stop","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"content":[{"type":"providerNative","subtype":"image_generation_call","raw":{"type":"image_generation_call","status":"completed","id":"same","result":result}}]})).expect("valid assistant message")
}
#[test]
fn repeated_ids_never_overwrite_existing_bytes() {
    let cwd = tempfile::tempdir().expect("temp dir");
    let first = externalize_native_images(&message("/9j/"), cwd.path()).expect("externalized");
    let second = externalize_native_images(&message("AQID"), cwd.path()).expect("externalized");
    externalize_native_images(&message("/9j/"), cwd.path()).expect("duplicate externalized");
    assert_eq!(std::fs::read(cwd.path().join("generated-images/same.jpg")).expect("first"), [255,216,255]);
    assert_eq!(std::fs::read(cwd.path().join("generated-images/same-2.jpg")).expect("duplicate"), [255,216,255]);
    assert!(!serde_json::to_string(&first).expect("json").contains("/9j/"));
    assert!(!serde_json::to_string(&second).expect("json").contains("AQID"));
}
#[test]
fn decode_failure_scrubs_bytes_and_second_pass_is_unchanged() {
    let cwd = tempfile::tempdir().expect("temp dir");
    let scrubbed = externalize_native_images(&message("!!"), cwd.path()).expect("scrubbed");
    assert!(!serde_json::to_string(&scrubbed).expect("json").contains("result"));
    assert!(externalize_native_images(&scrubbed, cwd.path()).is_none());
}
#[test]
fn disk_failure_scrubs_payload() {
    let cwd = tempfile::tempdir().expect("temp dir");
    std::fs::write(cwd.path().join("generated-images"), b"occupied").expect("obstruct directory");
    let scrubbed = externalize_native_images(&message("AQID"), cwd.path()).expect("scrubbed");
    assert!(!serde_json::to_string(&scrubbed).expect("json").contains("AQID"));
}
#[test]
fn only_completed_matching_calls_and_nonblank_prompts_are_recognized() {
    assert!(read_completed_native_image(&json!({"type":"image_generation_call","status":"in_progress","result":"AQID"})).is_none());
    let raw = json!({"type":"image_generation_call","status":"completed","result":"AQID","revised_prompt":"  prompt  "});
    assert_eq!(read_completed_native_image(&raw).expect("completed").revised_prompt, Some("  prompt  "));
    assert_eq!(detect_image_extension(b"RIFFxxxxWEBP"), "webp");
    assert_eq!(detect_image_extension(b"unknown"), "png");
}
