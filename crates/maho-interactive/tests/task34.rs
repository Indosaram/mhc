//! Todo 34 QA: ask-user, onboarding, grok chrome and tips parity against the pinned senpi goldens.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Once;

use maho_interactive::components::ask_user_async_widget::{
    build_comment_response, build_timed_out_response, render_status_line, unanswered_ids,
};
use maho_interactive::components::ask_user_question::{
    AskUserQuestionComponent, AskUserQuestionOptions,
};
use maho_interactive::components::ask_user_question_state::{
    format_countdown_label, QuestionDraft, QuestionRequest, QuestionResponse, NOT_ANSWERED_NOTICE,
};
use maho_interactive::components::first_time_setup::{
    FirstTimeSetupComponent, FirstTimeSetupOptions,
};
use maho_interactive::grok::tool_row::{GrokToolRow, GrokToolRowState};
use maho_interactive::grok::welcome_card::GrokWelcomeCard;
use maho_interactive::theme::{ColorMode, TerminalTheme, Theme};
use maho_interactive::tips::favorite_messages::{
    build_favorite_cycle_status_message, FavoriteCycleStatusKind,
};
use maho_interactive::tips::history_writer::record_tip_shown;
use maho_interactive::tips::registry::TIP_DEFINITIONS;
use maho_interactive::tips::scheduler::{select_tip, SelectTipOptions};
use maho_interactive::tips::startup_tip::{resolve_startup_tip_line, StartupTipOptions};
use maho_interactive::tips::working_tip::{resolve_working_tip_line, WorkingTipOptions};
use maho_tui::keybindings::get_keybindings;
use maho_tui::tui::Component;
use serde_json::Value;

fn theme() -> Theme {
    Theme::builtin("dark", ColorMode::Truecolor).expect("dark theme builds")
}

static KEYBINDINGS: Once = Once::new();

fn install_keybindings() {
    KEYBINDINGS.call_once(|| {
        let manager = maho_core::keybindings::KeybindingsManager::new(
            maho_tui::keybindings::KeybindingsConfig::new(),
            None,
        );
        maho_tui::keybindings::set_keybindings(manager.inner().clone());
    });
}

fn golden(name: &str) -> Value {
    let path = format!("{}/tests/golden/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path}: {error}"));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("parse {path}: {error}"))
}

fn key_string(binding: &str) -> String {
    let keys = get_keybindings().get_keys(binding);
    if keys.is_empty() {
        String::new()
    } else {
        keys.join("/")
    }
}

#[test]
fn ask_user_question_matches_pinned_golden() {
    install_keybindings();
    let fixture = golden("task34-ask.json");

    let request: QuestionRequest =
        serde_json::from_value(fixture["happy"]["request"].clone()).expect("request parses");
    let captured: Rc<RefCell<Option<QuestionResponse>>> = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&captured);
    let mut component = AskUserQuestionComponent::new(
        request,
        Box::new(move |response| *sink.borrow_mut() = Some(response)),
        AskUserQuestionOptions::new(theme()),
    );
    component.handle_input("\x1b[B");
    component.handle_input("\r");
    let response = captured
        .borrow()
        .clone()
        .expect("arrow-down + Enter resolves the single-question request");
    assert_eq!(
        serde_json::to_value(&response).expect("response serializes"),
        fixture["happy"]["response"],
        "happy path response"
    );

    let notice_request: QuestionRequest =
        serde_json::from_value(fixture["happy"]["request"].clone()).expect("request parses");
    let notice_captured: Rc<RefCell<Option<QuestionResponse>>> = Rc::new(RefCell::new(None));
    let notice_sink = Rc::clone(&notice_captured);
    let mut notice_component = AskUserQuestionComponent::new(
        notice_request,
        Box::new(move |response| *notice_sink.borrow_mut() = Some(response)),
        AskUserQuestionOptions::new(theme()),
    );
    notice_component.handle_input("c");
    notice_component.handle_input("\r");
    assert_eq!(
        notice_component.state.notice.as_deref(),
        Some(NOT_ANSWERED_NOTICE)
    );
    assert_eq!(
        Value::from(notice_component.state.notice.clone().unwrap_or_default()),
        fixture["notice"]["notice"],
        "empty submit keeps the overlay open with the notice"
    );
    assert!(notice_captured.borrow().is_none(), "no response is produced yet");
}

#[test]
fn ask_user_timeout_and_countdown_match_pinned_golden() {
    install_keybindings();
    let fixture = golden("task34-ask.json");
    let request: QuestionRequest =
        serde_json::from_value(fixture["happy"]["request"].clone()).expect("request parses");
    let draft = QuestionDraft::default();

    let timed_out = build_timed_out_response(&request, &draft, 60000);
    assert_eq!(
        serde_json::to_value(&timed_out).expect("response serializes"),
        fixture["timeout"]["response"],
        "countdown expiry returns the senpi timeout answer"
    );

    let comment = build_comment_response(&request, &draft, "use react please");
    assert_eq!(
        serde_json::to_value(&comment).expect("response serializes"),
        fixture["timeout"]["commentResponse"],
        "composer text becomes the comment answer"
    );

    assert_eq!(
        serde_json::to_value(unanswered_ids(&request, &draft)).expect("ids serialize"),
        fixture["timeout"]["unanswered"]
    );

    let single = render_status_line(&theme(), 1, "01:00", 1);
    let multi = render_status_line(&theme(), 2, "05:00", 3);
    assert_eq!(Value::from(single), fixture["timeout"]["statusLine"]);
    assert_eq!(Value::from(multi), fixture["timeout"]["statusLineMulti"]);

    for case in fixture["timeout"]["countdownLabels"]
        .as_array()
        .expect("countdown cases")
    {
        let ms = case[0].as_f64().expect("milliseconds");
        let expected = case[1].as_str().expect("label");
        assert_eq!(format_countdown_label(ms), expected, "label for {ms}ms");
    }
}

#[test]
fn tips_registry_scheduler_and_lines_match_pinned_golden() {
    install_keybindings();
    let fixture = golden("task34-tips.json");

    let ids: Vec<Value> = TIP_DEFINITIONS
        .iter()
        .map(|tip| Value::from(tip.id))
        .collect();
    assert_eq!(Value::Array(ids), fixture["registryIds"], "registry order");

    for binding in ["app.thinking.cycle", "app.model.select", "app.models.toggleFavorite"] {
        assert_eq!(
            Value::from(key_string(binding)),
            fixture["keys"][binding],
            "key text for {binding}"
        );
    }

    let has_command = |command: &str| ["help", "diff", "memory", "tasks", "fallback"].contains(&command);
    for selection in fixture["selections"].as_array().expect("selections") {
        let history: HashMap<String, u64> = selection["history"]
            .as_object()
            .expect("history object")
            .iter()
            .map(|(key, value)| (key.clone(), value.as_u64().expect("timestamp")))
            .collect();
        let keys = key_string;
        let use_has_command = selection["hasCommand"].as_bool().expect("hasCommand flag");
        let tip = select_tip(
            &TIP_DEFINITIONS,
            &history,
            0,
            SelectTipOptions {
                exclude: None,
                keys: Some(&keys),
                has_command: if use_has_command {
                    Some(&has_command as &dyn Fn(&str) -> bool)
                } else {
                    None
                },
            },
        );
        let actual = tip.map_or(Value::Null, |tip| Value::from(tip.id));
        assert_eq!(actual, selection["tipId"], "scheduler pick");
    }

    let startup_history = HashMap::from([("thinking-level".to_string(), 10u64)]);
    let startup = resolve_startup_tip_line(StartupTipOptions {
        tips_enabled: true,
        quiet_startup: false,
        history: &startup_history,
        now: 0,
        definitions: &TIP_DEFINITIONS,
        keys: &key_string,
        has_command: None,
        exclude: None,
    })
    .expect("startup tip resolves");
    assert_eq!(Value::from(startup.line), fixture["startupTip"]["line"]);
    assert_eq!(Value::from(startup.tip_id), fixture["startupTip"]["tipId"]);

    let empty_history: HashMap<String, u64> = HashMap::new();
    assert!(
        resolve_startup_tip_line(StartupTipOptions {
            tips_enabled: false,
            quiet_startup: false,
            history: &empty_history,
            now: 0,
            definitions: &TIP_DEFINITIONS,
            keys: &key_string,
            has_command: None,
            exclude: None,
        })
        .is_none(),
        "disabled tips render nothing"
    );

    let working_history: HashMap<String, u64> = HashMap::new();
    let working = resolve_working_tip_line(WorkingTipOptions {
        tips_enabled: true,
        history: &working_history,
        session_shown_tip_ids: &HashSet::from(["thinking-level".to_string()]),
        now: 0,
        definitions: &TIP_DEFINITIONS,
        keys: &key_string,
        has_command: None,
    })
    .expect("working tip resolves");
    assert_eq!(Value::from(working.line), fixture["workingTip"]["line"]);
    assert_eq!(Value::from(working.tip_id), fixture["workingTip"]["tipId"]);

    let recorded = record_tip_shown(&HashMap::from([("a".to_string(), 1)]), "b", 7);
    assert_eq!(
        serde_json::to_value(&recorded).expect("history serializes"),
        fixture["historyRecorded"]
    );

    assert_eq!(
        Value::from(build_favorite_cycle_status_message(FavoriteCycleStatusKind::Empty)),
        fixture["favoriteEmpty"]
    );
    assert_eq!(
        Value::from(build_favorite_cycle_status_message(FavoriteCycleStatusKind::Single)),
        fixture["favoriteSingle"]
    );
}

#[test]
fn grok_chrome_and_first_time_setup_render_matches_pinned_golden() {
    install_keybindings();
    let fixture = golden("task34-render.json");
    let theme = theme();

    for width in [40usize, 60, 80, 120] {
        let welcome = GrokWelcomeCard::new("maho", "1.2.3", theme.clone()).render(width);
        assert_eq!(
            serde_json::to_value(&welcome).expect("lines serialize"),
            fixture[format!("welcome.{width}")],
            "welcome card at {width}"
        );

        let cases = [
            (
                "toolRow.partial",
                GrokToolRowState {
                    tool_name: "bash".to_string(),
                    is_partial: true,
                    is_error: None,
                },
            ),
            (
                "toolRow.error",
                GrokToolRowState {
                    tool_name: "bash".to_string(),
                    is_partial: false,
                    is_error: Some(true),
                },
            ),
            (
                "toolRow.ok",
                GrokToolRowState {
                    tool_name: "read".to_string(),
                    is_partial: false,
                    is_error: Some(false),
                },
            ),
        ];
        for (name, state) in cases {
            let row = GrokToolRow::new(state, theme.clone()).render(width);
            assert_eq!(
                serde_json::to_value(&row).expect("lines serialize"),
                fixture[format!("{name}.{width}")],
                "{name} at {width}"
            );
        }
    }

    let mut setup = FirstTimeSetupComponent::new(FirstTimeSetupOptions {
        detected_theme: TerminalTheme::Dark,
        theme: theme.clone(),
        on_theme_preview: Box::new(|_| {}),
        on_submit: Box::new(|_| {}),
        on_cancel: Box::new(|| {}),
    });
    for width in [40usize, 60, 80] {
        let lines = setup.render(width);
        assert_eq!(
            serde_json::to_value(&lines).expect("lines serialize"),
            fixture[format!("firstTimeSetup.theme.{width}")],
            "first-time setup theme step at {width}"
        );
    }
    setup.handle_input("\r");
    for width in [40usize, 60, 80] {
        let lines = setup.render(width);
        assert_eq!(
            serde_json::to_value(&lines).expect("lines serialize"),
            fixture[format!("firstTimeSetup.analytics.{width}")],
            "first-time setup analytics step at {width}"
        );
    }
}
