use maho_interactive::components::keybinding_hints::{
    format_key_text, key_display_text, key_hint, key_text, raw_key_hint,
};
use maho_interactive::components::todo_strike::{has_completed_todo_tasks, partial_strikethrough, strike_reveal_count};
use maho_interactive::theme::{ColorMode, Theme};
use serde_json::Value;

fn theme() -> Theme {
    Theme::builtin("dark", ColorMode::Color256).expect("builtin theme")
}

fn fixtures() -> Value {
    serde_json::from_str(include_str!("golden/components32-support.json")).expect("pinned fixture")
}

#[test]
fn todo_strike_reveal_counts_match_pinned_senpi() {
    for case in fixtures()["strike"].as_array().expect("strike") {
        if case.get("kind").and_then(Value::as_str) == Some("hasCompleted") {
            let details = case["details"].clone();
            assert_eq!(
                has_completed_todo_tasks(&details),
                case["hasCompleted"].as_bool().expect("hasCompleted"),
                "hasCompletedTodoTasks({details})"
            );
            continue;
        }
        let text = case["text"].as_str().expect("text");
        let frame = case["frame"].as_i64();
        assert_eq!(
            strike_reveal_count(text, frame),
            case["count"].as_i64(),
            "strikeRevealCount({text:?}, {frame:?})"
        );
    }
}

#[test]
fn partial_strikethrough_matches_pinned_senpi() {
    for case in fixtures()["partial"].as_array().expect("partial") {
        let text = case["text"].as_str().expect("text");
        let visible = case["visible"].as_i64().expect("visible");
        assert_eq!(
            partial_strikethrough(text, visible, |value| format!("<{value}>")),
            case["output"].as_str().expect("output"),
            "partialStrikethrough({text:?}, {visible})"
        );
    }
}

#[test]
fn key_text_formatting_matches_pinned_senpi() {
    for case in fixtures()["keyText"].as_array().expect("keyText") {
        let key = case["key"].as_str().expect("key");
        let capitalize = case["capitalize"].as_bool().expect("capitalize");
        assert_eq!(
            format_key_text(key, capitalize),
            case["output"].as_str().expect("output"),
            "formatKeyText({key:?}, capitalize={capitalize})"
        );
    }
}

#[test]
fn keybinding_hints_match_pinned_senpi_for_registered_bindings() {
    let theme = theme();
    for case in fixtures()["keyHint"].as_array().expect("keyHint") {
        let keybinding = case["keybinding"].as_str().expect("keybinding");
        let description = case["description"].as_str().expect("description");
        assert_eq!(key_text(keybinding), case["keyText"].as_str().expect("keyText"), "keyText({keybinding})");
        assert_eq!(
            key_display_text(keybinding),
            case["keyDisplayText"].as_str().expect("keyDisplayText"),
            "keyDisplayText({keybinding})"
        );
        assert_eq!(
            key_hint(keybinding, description, &theme),
            case["keyHint"].as_str().expect("keyHint"),
            "keyHint({keybinding})"
        );
    }
}

#[test]
fn raw_key_hints_match_pinned_senpi() {
    let theme = theme();
    for case in fixtures()["rawKey"].as_array().expect("rawKey") {
        let key = case["key"].as_str().expect("key");
        let description = case["description"].as_str().expect("description");
        assert_eq!(
            raw_key_hint(key, description, &theme),
            case["output"].as_str().expect("output"),
            "rawKeyHint({key:?})"
        );
    }
}
