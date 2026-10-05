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

const PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl3T2QAAAAASUVORK5CYII=";

fn native_block(id: Option<&str>, status: &str, result: Option<&str>, revised: Option<&str>) -> serde_json::Value {
    let mut raw = json!({"type":"image_generation_call","status":status});
    if let Some(id) = id { raw["id"] = json!(id); }
    if let Some(result) = result { raw["result"] = json!(result); }
    if let Some(revised) = revised { raw["revised_prompt"] = json!(revised); }
    json!({"type":"providerNative","subtype":"image_generation_call","raw":raw})
}

fn assistant(content: Vec<serde_json::Value>, response_id: Option<&str>) -> AssistantMessage {
    let mut value = json!({"role":"assistant","api":"openai-responses","provider":"openai","model":"model","timestamp":0,"stopReason":"stop","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"content":content});
    if let Some(id) = response_id { value["responseId"] = json!(id); }
    serde_json::from_value(value).expect("assistant")
}

#[test]
fn completed_call_saves_a_path_and_reports_the_revised_prompt() {
    let cwd = tempfile::tempdir().expect("temp dir");
    let message = assistant(vec![native_block(Some("ig_fixture_1"), "completed", Some(PNG_BASE64), Some("a red fox in snow"))], None);
    let replaced = externalize_native_images(&message, cwd.path()).expect("externalized");
    let text = serde_json::to_string(&replaced).expect("json");
    assert!(text.contains("Generated image: generated-images/ig_fixture_1.png"));
    assert!(text.contains("Revised prompt: a red fox in snow"));
    let saved = std::fs::read(cwd.path().join("generated-images/ig_fixture_1.png")).expect("saved");
    assert_eq!(saved[..8], [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    assert!(!text.contains("iVBORw0KGgo"));
}

#[test]
fn missing_item_id_uses_the_response_id_and_output_index() {
    let cwd = tempfile::tempdir().expect("temp dir");
    let message = assistant(vec![native_block(None, "completed", Some(PNG_BASE64), None)], Some("resp_42"));
    let replaced = externalize_native_images(&message, cwd.path()).expect("externalized");
    assert!(serde_json::to_string(&replaced).expect("json").contains("generated-images/resp_42-0.png"));
    assert!(cwd.path().join("generated-images/resp_42-0.png").exists());
}

#[test]
fn a_duplicate_id_in_one_message_suffixes_and_keeps_both() {
    let cwd = tempfile::tempdir().expect("temp dir");
    let message = assistant(vec![native_block(Some("ig_dup"), "completed", Some(PNG_BASE64), None), native_block(Some("ig_dup"), "completed", Some(PNG_BASE64), None)], None);
    let replaced = externalize_native_images(&message, cwd.path()).expect("externalized");
    let text = serde_json::to_string(&replaced).expect("json");
    assert!(text.contains("generated-images/ig_dup.png"));
    assert!(text.contains("generated-images/ig_dup-2.png"));
    assert!(cwd.path().join("generated-images/ig_dup.png").exists());
    assert!(cwd.path().join("generated-images/ig_dup-2.png").exists());
}
