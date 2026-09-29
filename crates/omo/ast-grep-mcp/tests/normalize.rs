use ast_grep_mcp::normalize::normalize_match;
use ast_grep_mcp::normalize::normalize_records;
use pretty_assertions::assert_eq;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

const WORKDIR: &str = "/workspace/project";

fn record(file: &str, byte_offset: u64, text: &str) -> Map<String, Value> {
    let value = json!({
        "text": text,
        "range": {
            "byteOffset": { "start": byte_offset, "end": byte_offset + text.len() as u64 },
            "start": { "line": 2, "column": 4 },
            "end": { "line": 2, "column": 4 + text.encode_utf16().count() },
        },
        "file": file,
        "lines": format!("{text};"),
        "language": "TypeScript",
        "metaVariables": {
            "single": {
                "CALLEE": {
                    "text": "console.log",
                    "range": {
                        "byteOffset": { "start": byte_offset, "end": byte_offset + 11 },
                        "start": { "line": 2, "column": 4 },
                        "end": { "line": 2, "column": 15 },
                    },
                },
            },
            "multi": {
                "ARGS": [
                    { "text": "a", "range": { "byteOffset": { "start": byte_offset + 12, "end": byte_offset + 13 } } },
                    { "text": ",", "range": { "byteOffset": { "start": byte_offset + 13, "end": byte_offset + 14 } } },
                    { "text": "b", "range": { "byteOffset": { "start": byte_offset + 15, "end": byte_offset + 16 } } },
                ],
                "EMPTY": [],
            },
            "transformed": {},
        },
        "replacement": "logger.info(a, b)",
        "ruleId": "no-console",
    });
    match value {
        Value::Object(map) => map,
        other => panic!("fixture is not an object: {other}"),
    }
}

fn default_record(file: &str, byte_offset: u64) -> Map<String, Value> {
    record(file, byte_offset, "console.log(a, b)")
}

fn path_of(file: &str, workdir: &str) -> Value {
    let normalized = normalize_match(&default_record(file, 0), workdir).expect("normalize");
    normalized["path"].clone()
}

#[test]
fn pinned_record_normalizes_to_stable_contract() {
    let normalized = normalize_match(&default_record("/workspace/project/src/a.ts", 100), WORKDIR)
        .expect("normalize");
    assert_eq!(
        Value::Object(normalized),
        json!({
            "path": "src/a.ts",
            "language": "typescript",
            "text": "console.log(a, b)",
            "range": {
                "start": { "line": 3, "column": 4, "byteOffset": 100 },
                "end": { "line": 3, "column": 21, "byteOffset": 117 },
            },
            "metavariables": {
                "single": { "CALLEE": "console.log" },
                "multi": { "ARGS": "a, b", "EMPTY": "" },
            },
            "replacement": "logger.info(a, b)",
            "ruleId": "no-console",
        })
    );
}

#[test]
fn inside_outside_and_windows_paths_are_stable() {
    assert_eq!(path_of("/other/a.ts", WORKDIR), json!("/other/a.ts"));
    assert_eq!(
        path_of("C:\\repo\\src\\a.ts", "C:\\repo"),
        json!("src/a.ts")
    );
    assert_eq!(
        path_of("D:\\other\\a.ts", "C:\\repo"),
        json!("D:/other/a.ts")
    );
}

#[test]
fn unsorted_records_sort_by_path_and_byte_offset() {
    let normalized = normalize_records(
        &[
            default_record("/workspace/project/z.ts", 1),
            default_record("/workspace/project/a.ts", 20),
            default_record("/workspace/project/a.ts", 3),
        ],
        WORKDIR,
    )
    .expect("normalize");
    let keys: Vec<Value> = normalized
        .iter()
        .map(|entry| json!([entry["path"], entry["range"]["start"]["byteOffset"]]))
        .collect();
    assert_eq!(
        keys,
        vec![json!(["a.ts", 3]), json!(["a.ts", 20]), json!(["z.ts", 1])]
    );
}

#[test]
fn multibyte_multi_capture_uses_utf8_byte_offsets() {
    let mut raw = record("/workspace/project/a.ts", 10, "call(α, β)");
    raw["metaVariables"]["multi"]["ARGS"] = json!([
        { "text": "α", "range": { "byteOffset": { "start": 15, "end": 17 } } },
        { "text": ",", "range": { "byteOffset": { "start": 17, "end": 18 } } },
        { "text": "β", "range": { "byteOffset": { "start": 19, "end": 21 } } },
    ]);
    let normalized = normalize_match(&raw, WORKDIR).expect("normalize");
    assert_eq!(normalized["metavariables"]["multi"]["ARGS"], json!("α, β"));
}
