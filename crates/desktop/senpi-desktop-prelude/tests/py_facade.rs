use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};

use senpi_desktop_prelude::ComputerPreludeAssets;

const WINDOW_SNAPSHOT: &str = r#"{"id":"w1","app":"Code","title":"main.ts","pid":42,"bounds":{"x":0,"y":0,"width":800,"height":600},"focused":true}"#;
const ELEMENT_SNAPSHOT: &str = r#"{"ref":"e1","role":"button","nativeRole":"AXButton","enabled":true,"focused":false,"childCount":0}"#;
const IMAGE: &str = r#"{"mimeType":"image/png","dataBase64":"iVBORw0KGgo="}"#;

const PYTHON_DRIVER: &str = r#"
import json, sys
payload = json.load(sys.stdin)
calls, displayed = [], []
WINDOW = json.loads(payload["window"])
ELEMENT = json.loads(payload["element"])
IMAGE = json.loads(payload["image"])

class _Tool:
    def computer(self, **args):
        calls.append(args)
        chain = args.get("chain") or []
        last = chain[-1]["method"] if chain else None
        override = payload.get("responders", {}).get(last)
        if override is not None:
            return json.loads(override)
        if len(chain) == 1 and last == "window":
            return {"text": "", "details": {"value": WINDOW}, "images": [], "hasError": False}
        if len(chain) == 1 and last == "ref":
            return {"text": "", "details": {"value": ELEMENT}, "images": [], "hasError": False}
        if last == "clipboard.write":
            return {"text": "PermissionDenied: computer:exec is denied", "images": [], "hasError": True}
        if last == "screenshot":
            return {"text": "", "details": {"value": {"width": 1}}, "images": [IMAGE], "hasError": False}
        return {"text": ""}

ns = {"tool": _Tool(), "display": displayed.append}
exec(payload["prelude"], ns)
error = None
try:
    exec(payload["script"], ns)
except Exception as caught:
    error = f"{type(caught).__name__}: {caught}"

print(json.dumps({"calls": calls, "displayed": displayed, "out": ns.get("out"), "error": error}))
"#;

fn run_python_test(script: &str, responders: HashMap<String, String>) -> Value {
    let input = json!({
        "prelude": ComputerPreludeAssets::python(),
        "script": script,
        "window": WINDOW_SNAPSHOT,
        "element": ELEMENT_SNAPSHOT,
        "image": IMAGE,
        "responders": responders,
    });

    let python_cmd = if cfg!(windows) { "python" } else { "python3" };
    let mut child = Command::new(python_cmd)
        .arg("-c")
        .arg(PYTHON_DRIVER)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn python runner");

    let input_bytes = serde_json::to_vec(&input).unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&input_bytes)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "Python runner failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn py_turns_keyword_arguments_into_one_trailing_options_object() {
    let res = run_python_test(
        "win = computer.window(app='Code')\nwin.click(10, 20, button='right', delivery=None)",
        HashMap::new(),
    );
    assert!(res["error"].is_null(), "facade error: {}", res["error"]);
    let calls = res["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0],
        json!({ "action": "call", "chain": [{ "method": "window", "args": [{ "app": "Code" }] }] })
    );
    assert_eq!(
        calls[1],
        json!({
            "action": "call",
            "chain": [
                { "method": "window", "args": ["w1"] },
                { "method": "click", "args": [10, 20, { "button": "right" }] }
            ]
        })
    );
}

#[test]
fn py_drops_trailing_none_positionals_and_sends_no_empty_options_object() {
    let res = run_python_test("computer.scroll(3, 4, None)", HashMap::new());
    let calls = res["calls"].as_array().unwrap();
    assert_eq!(
        calls,
        &vec![json!({ "action": "call", "chain": [{ "method": "scroll", "args": [3, 4] }] })]
    );
}

#[test]
fn py_sends_raise_as_the_raise_method() {
    let res = run_python_test("computer.window(app='Code').raise_()", HashMap::new());
    let calls = res["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[1],
        json!({
            "action": "call",
            "chain": [
                { "method": "window", "args": ["w1"] },
                { "method": "raise", "args": [] }
            ]
        })
    );
}

#[test]
fn py_returns_details_value_and_displays_every_result_image() {
    let res = run_python_test("out = computer.screenshot(silent=False)", HashMap::new());
    assert_eq!(res["out"], json!({ "width": 1 }));
    assert_eq!(res["displayed"].as_array().unwrap().len(), 1);
    assert_eq!(
        res["displayed"][0],
        serde_json::from_str::<Value>(IMAGE).unwrap()
    );
}

#[test]
fn py_raises_runtime_error_with_the_result_text_when_the_tool_reports_an_error() {
    let res = run_python_test("computer.clipboard.write('secret')", HashMap::new());
    assert_eq!(
        res["error"],
        "RuntimeError: PermissionDenied: computer:exec is denied"
    );
}

#[test]
fn py_raises_runtime_error_with_the_computer_permission_required_text_when_a_permission_denial_comes_back_as_an_error()
 {
    let text = "COMPUTER_PERMISSION_REQUIRED: screen recording is not granted. Do not retry until the user says the grant is done and the app was relaunched.";
    let denial = json!({
        "text": text,
        "details": { "value": { "code": "COMPUTER_PERMISSION_REQUIRED" }, "isError": true },
        "hasError": true,
        "images": []
    })
    .to_string();

    let mut responders = HashMap::new();
    responders.insert("screenshot".to_string(), denial);

    let res = run_python_test("computer.screenshot()", responders);
    assert_eq!(res["error"], format!("RuntimeError: {}", text));
}

#[test]
fn py_exposes_the_resolved_window_identity_fields_on_the_handle() {
    let res = run_python_test(
        "w = computer.window(app='Code')\nout = {f: getattr(w, f) for f in ('id', 'app', 'title', 'pid', 'bounds', 'focused')}",
        HashMap::new(),
    );
    assert_eq!(
        res["out"],
        serde_json::from_str::<Value>(WINDOW_SNAPSHOT).unwrap()
    );
}

#[test]
fn py_sends_computer_run_options_only_when_set() {
    let res = run_python_test("computer.run('return 1;', read_only=True)", HashMap::new());
    let calls = res["calls"].as_array().unwrap();
    assert_eq!(
        calls,
        &vec![json!({ "action": "run", "code": "return 1;", "read_only": true })]
    );
}

#[test]
fn py_refuses_a_python_callable_for_computer_run_before_calling_the_tool() {
    let res = run_python_test("computer.run(lambda: 1)", HashMap::new());
    assert_eq!(
        res["error"],
        "TypeError: computer.run() expects a JavaScript code string"
    );
    assert!(res["calls"].as_array().unwrap().is_empty());
}
