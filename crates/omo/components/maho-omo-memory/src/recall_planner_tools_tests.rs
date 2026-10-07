use super::*;
use serde_json::json;

#[test]
fn given_a_command_when_harvested_then_path_words_precede_the_command_name() {
    let texts = tool_arg_texts("bash", &json!({ "command": "git log src/sync/mirror.rs" }));
    assert!(texts.contains(&"mirror.rs".to_string()));
    assert!(texts.contains(&"git".to_string()));
    let mirror = texts.iter().position(|text| text == "mirror.rs").unwrap();
    let git = texts.iter().position(|text| text == "git").unwrap();
    assert!(mirror < git, "path words precede the command name: {texts:?}");
}

#[test]
fn given_secret_like_material_when_harvested_then_it_is_dropped() {
    let texts = tool_arg_texts("bash", &json!({ "command": "curl -H 'Authorization: Bearer sk-abc123' https://x" }));
    assert!(texts.iter().all(|text| !text.contains("sk-abc123")));
}

#[test]
fn given_a_path_key_when_harvested_then_its_file_words_are_used() {
    let texts = tool_arg_texts("read", &json!({ "file_path": "notes/deploy-gate.md" }));
    assert!(texts.contains(&"deploy-gate.md".to_string()));
    assert!(texts.contains(&"deploy".to_string()));
    assert!(texts.contains(&"gate".to_string()));
}

#[test]
fn given_an_array_key_when_harvested_then_each_item_contributes_words() {
    let texts = tool_arg_texts("read", &json!({ "paths": ["a/one.md", "b/two.md"] }));
    assert!(texts.contains(&"one.md".to_string()));
    assert!(texts.contains(&"two.md".to_string()));
}

#[test]
fn given_a_summary_when_harvested_then_the_value_and_its_path_words_are_used() {
    let texts = tool_arg_texts("eval", &json!({ "summary": "checking src/recall/gate.rs now" }));
    assert!(texts.contains(&"checking src/recall/gate.rs now".to_string()));
    assert!(texts.contains(&"gate.rs".to_string()));
}

#[test]
fn given_more_than_the_cap_when_harvested_then_the_result_is_capped() {
    let command = (0..64).map(|index| format!("src/f{index}.rs")).collect::<Vec<_>>().join(" ");
    let texts = tool_arg_texts("bash", &json!({ "command": command }));
    assert!(texts.len() <= 32);
}

#[test]
fn given_a_window_when_pushed_past_the_cap_then_only_the_last_pushes_remain() {
    let mut window = ToolArgWindow::new();
    for index in 0..(TOOL_ARG_WINDOW + 2) {
        window.push("s", vec![format!("t{index}")]);
    }
    let texts = window.texts("s");
    assert_eq!(texts.len(), TOOL_ARG_WINDOW);
    assert!(!texts.contains(&"t0".to_string()));
    assert!(!texts.contains(&"t1".to_string()));
    window.clear("s");
    assert!(window.texts("s").is_empty());
}
