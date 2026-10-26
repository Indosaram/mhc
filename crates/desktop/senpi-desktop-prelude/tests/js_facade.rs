use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Stdio};

use senpi_desktop_prelude::ComputerPreludeAssets;

const WINDOW_SNAPSHOT: &str = r#"{"id":"w1","app":"Code","title":"main.ts","pid":42,"bounds":{"x":0,"y":0,"width":800,"height":600},"focused":true}"#;
const IMAGE: &str = r#"{"mimeType":"image/png","dataBase64":"iVBORw0KGgo="}"#;

const JS_DRIVER: &str = r#"
const fs = require('fs');
const payload = JSON.parse(fs.readFileSync(0, 'utf8'));
const calls = [];
const displayed = [];
const WINDOW = JSON.parse(payload.window);
const IMAGE = JSON.parse(payload.image);

const defaultResponder = (args) => {
  const chain = Array.isArray(args.chain) ? args.chain : [];
  const root = chain.length === 1 ? chain[0].method : undefined;
  if (root === 'window') return { text: '', details: { value: WINDOW } };
  if (root === 'ref') return { text: '', details: { value: { ref: 'e1', role: 'button' } } };
  return { text: '' };
};

const kernelGlobal = {
  tool: {
    computer: async (args) => {
      const wire = JSON.parse(JSON.stringify(args));
      calls.push(wire);
      if (payload.responder) {
        const fn = new Function('args', 'WINDOW', 'IMAGE', payload.responder);
        return fn(wire, WINDOW, IMAGE);
      }
      return defaultResponder(wire);
    }
  },
  display: (val) => displayed.push(val)
};

new Function('globalThis', payload.prelude)(kernelGlobal);

(async () => {
  try {
    const AsyncFunction = Object.getPrototypeOf(async () => {}).constructor;
    const runner = new AsyncFunction('computer', 'calls', 'displayed', 'IMAGE', 'WINDOW', payload.script);
    const out = await runner(kernelGlobal.computer, calls, displayed, IMAGE, WINDOW);
    process.stdout.write(JSON.stringify({ calls, displayed, out, error: null }));
  } catch (err) {
    process.stdout.write(JSON.stringify({ calls, displayed, out: null, error: err.message || String(err) }));
  }
})();
"#;

fn js_command() -> String {
    if Command::new("bun").arg("--version").output().is_ok() {
        "bun".to_string()
    } else {
        "node".to_string()
    }
}

fn run_js_test(script: &str, responder: Option<&str>) -> Value {
    let input = json!({
        "prelude": ComputerPreludeAssets::javascript(),
        "script": script,
        "window": WINDOW_SNAPSHOT,
        "image": IMAGE,
        "responder": responder,
    });

    let cmd = js_command();
    let mut child = Command::new(&cmd)
        .arg("-e")
        .arg(JS_DRIVER)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|_| panic!("Failed to spawn {}", cmd));

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
        "JS runner failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn js_sends_a_window_click_as_one_two_step_call_chain_through_tool_computer() {
    let res = run_js_test(
        r#"
        const win = await computer.window({ app: "Code" });
        await win.click(1, 2, { delivery: "foreground" });
        "#,
        None,
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
                { "method": "click", "args": [1, 2, { "delivery": "foreground" }] }
            ]
        })
    );
}

#[test]
fn js_exposes_the_resolved_window_identity_fields_on_the_handle() {
    let res = run_js_test(
        r#"
        return JSON.stringify(await computer.window({ app: "Code" }));
        "#,
        None,
    );
    let out_str = res["out"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(out_str).unwrap();
    let expected: Value = serde_json::from_str(WINDOW_SNAPSHOT).unwrap();
    assert_eq!(parsed, expected);
}

#[test]
fn js_returns_details_value_and_displays_every_result_image() {
    let res = run_js_test(
        r#"
        return await computer.screenshot();
        "#,
        Some(
            "return { text: 'screenshot desktop 1x1', details: { value: { width: 1 } }, images: [IMAGE] };",
        ),
    );
    assert_eq!(res["out"], json!({ "width": 1 }));
    assert_eq!(res["displayed"].as_array().unwrap().len(), 1);
    assert_eq!(
        res["displayed"][0],
        serde_json::from_str::<Value>(IMAGE).unwrap()
    );
}

#[test]
fn js_throws_the_result_text_when_the_tool_reports_an_error() {
    let res = run_js_test(
        r#"
        await computer.click(5, 5);
        "#,
        Some("return { text: 'Suspended: the user pressed the stop chord', hasError: true };"),
    );
    let err = res["error"].as_str().unwrap();
    assert!(err.contains("Suspended: the user pressed the stop chord"));
}

#[test]
fn js_throws_the_computer_permission_required_text_when_a_permission_denial_comes_back_as_an_error()
{
    let res = run_js_test(
        r#"
        await computer.screenshot();
        "#,
        Some(
            r#"
        const text = 'COMPUTER_PERMISSION_REQUIRED: accessibility permission is missing. Do not retry until the user says the grant is done and the app was relaunched.';
        return { text, details: { value: { code: 'COMPUTER_PERMISSION_REQUIRED' }, isError: true }, hasError: true };
        "#,
        ),
    );
    let err = res["error"].as_str().unwrap();
    assert!(err.contains("COMPUTER_PERMISSION_REQUIRED"));
}

#[test]
fn js_drops_trailing_undefined_arguments_from_a_chain_step() {
    let res = run_js_test(
        r#"
        await computer.scroll(3, 4, undefined);
        "#,
        None,
    );
    let calls = res["calls"].as_array().unwrap();
    assert_eq!(
        calls,
        &vec![json!({ "action": "call", "chain": [{ "method": "scroll", "args": [3, 4] }] })]
    );
}

#[test]
fn js_rejects_a_function_argument_to_a_direct_helper_before_calling_the_tool() {
    let res = run_js_test(
        r#"
        await computer.windows(() => true);
        "#,
        None,
    );
    assert!(res["error"].is_string());
    assert!(res["calls"].as_array().unwrap().is_empty());
}

#[test]
fn js_never_sends_a_three_step_chain_a_window_handle_has_no_window_to_chain() {
    let res = run_js_test(
        r#"
        const win = await computer.window({ app: "Code" });
        await win.window({ app: "Code" });
        "#,
        None,
    );
    assert!(res["error"].is_string());
    let calls = res["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0],
        json!({ "action": "call", "chain": [{ "method": "window", "args": [{ "app": "Code" }] }] })
    );
}

#[test]
fn js_serializes_computer_run_fn_into_code_that_calls_fn_with_the_run_scope_and_the_args() {
    let res = run_js_test(
        r#"
        await computer.run(async ({ desktop }, name, pattern, pick) => pick(await desktop.windows(), name, pattern), {
            args: ["Code", /main\.ts$/i, (list, name, pattern) => list.filter((w) => w.app === name && pattern.test(w.title))],
            read_only: true,
            timeout: 30,
        });
        "#,
        Some("return { text: '' };"),
    );
    let calls = res["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call["action"], "run");
    assert_eq!(call["read_only"], true);
    assert_eq!(call["timeout"], 30);
    assert!(call["code"].is_string());
}

#[test]
fn js_sends_a_code_string_to_computer_run_unchanged() {
    let res = run_js_test(
        r#"
        return await computer.run("return 7;");
        "#,
        Some("return { text: '', details: { value: 7 } };"),
    );
    assert_eq!(res["out"], 7);
    let calls = res["calls"].as_array().unwrap();
    assert_eq!(
        calls,
        &vec![json!({ "action": "run", "code": "return 7;" })]
    );
}

#[test]
fn js_maps_capabilities_and_close_to_their_own_tool_actions() {
    let res = run_js_test(
        r#"
        await computer.capabilities();
        await computer.close();
        "#,
        Some("return { text: '' };"),
    );
    let calls = res["calls"].as_array().unwrap();
    assert_eq!(
        calls,
        &vec![
            json!({ "action": "capabilities" }),
            json!({ "action": "close" })
        ]
    );
}
