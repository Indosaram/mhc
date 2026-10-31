//! Translation of `fable-55-launch.test.ts`.

use model_core::FABLE_55_EFFORTS;
use model_core::FABLE_55_LAUNCH;
use model_core::FABLE_55_LAUNCH_ID;
use model_core::FABLE_55_RELEASED;
use model_core::FallbackEntry;
use model_core::LaunchGate;
use model_core::LaunchRungRejection;
use model_core::ModelCapabilitiesSnapshotEntry;
use model_core::SnapshotLimit;
use model_core::SnapshotModalities;
use model_core::latest_models::LATEST_MODEL_BY_FAMILY;
use model_core::launch_default_model_id;
use model_core::select_launch_rungs;
use pretty_assertions::assert_eq;

// Names every launch value still at its placeholder (None). Rust structs have a fixed field set,
// so "any field added later" is covered by the compiler instead of a dynamic key walk.
fn unfilled_launch_values(entry: &ModelCapabilitiesSnapshotEntry) -> Vec<&'static str> {
    let modalities = entry.modalities.as_ref();
    let limit = entry.limit.as_ref();
    let mut unfilled = Vec::new();
    let checks: [(&'static str, bool); 9] = [
        ("family", entry.family.is_none()),
        ("limit.context", limit.and_then(|l| l.context).is_none()),
        ("limit.input", limit.and_then(|l| l.input).is_none()),
        ("limit.output", limit.and_then(|l| l.output).is_none()),
        (
            "modalities.input",
            modalities.and_then(|m| m.input.as_ref()).is_none(),
        ),
        (
            "modalities.output",
            modalities.and_then(|m| m.output.as_ref()).is_none(),
        ),
        ("reasoning", entry.reasoning.is_none()),
        ("temperature", entry.temperature.is_none()),
        ("toolCall", entry.tool_call.is_none()),
    ];
    for (key, missing) in checks {
        if missing {
            unfilled.push(key);
        }
    }
    unfilled
}

fn filled() -> ModelCapabilitiesSnapshotEntry {
    ModelCapabilitiesSnapshotEntry {
        id: "fixture".to_string(),
        family: Some("claude-fable".to_string()),
        reasoning: Some(true),
        temperature: Some(true),
        tool_call: Some(true),
        modalities: Some(SnapshotModalities {
            input: Some(vec!["text".to_string()]),
            output: Some(vec!["text".to_string()]),
        }),
        limit: Some(SnapshotLimit {
            context: Some(1),
            input: Some(1),
            output: Some(1),
        }),
    }
}

fn rung(model: &str, variant: Option<&str>) -> FallbackEntry {
    FallbackEntry {
        model: model.to_string(),
        variant: variant.map(str::to_string),
        ..FallbackEntry::default()
    }
}

#[test]
fn a_release_leaves_no_launch_value_at_its_placeholder() {
    let unfilled = if FABLE_55_RELEASED {
        unfilled_launch_values(&FABLE_55_LAUNCH)
    } else {
        Vec::new()
    };
    assert_eq!(unfilled, Vec::<&str>::new());
}

#[test]
fn the_placeholder_check_names_every_unfilled_value_and_passes_a_filled_entry() {
    assert_eq!(unfilled_launch_values(&filled()), Vec::<&str>::new());
    let mut bare = ModelCapabilitiesSnapshotEntry {
        id: "fixture".to_string(),
        ..ModelCapabilitiesSnapshotEntry::default()
    };
    bare.family = Some("claude-fable".to_string());
    assert_eq!(
        unfilled_launch_values(&bare),
        vec![
            "limit.context",
            "limit.input",
            "limit.output",
            "modalities.input",
            "modalities.output",
            "reasoning",
            "temperature",
            "toolCall",
        ]
    );
    let no_family = ModelCapabilitiesSnapshotEntry {
        family: None,
        ..filled()
    };
    assert_eq!(unfilled_launch_values(&no_family), vec!["family"]);
}

#[test]
fn a_release_measures_the_5_5_efforts_instead_of_carrying_them_over_from_5_1() {
    let released = std::hint::black_box(FABLE_55_RELEASED);
    assert!(!released || FABLE_55_EFFORTS.measured);
}

#[test]
fn a_released_5_5_rung_with_an_unaccepted_effort_is_dropped_and_recorded_never_thrown() {
    let rungs = vec![
        rung("anthropic/claude-fable-5.5", Some("max")),
        rung("claude-fable-5-5", Some("high")),
        rung("claude-fable-5-5", None),
        rung("claude-fable-5-1", Some("low")),
    ];
    let mut rejected = Vec::new();

    let selected = select_launch_rungs(
        rungs,
        LaunchGate {
            released: true,
            accepted: &["xhigh", "max"],
        },
        |rejection| rejected.push(rejection),
    );

    assert_eq!(
        selected
            .iter()
            .map(|r| r.model.as_str())
            .collect::<Vec<_>>(),
        vec!["anthropic/claude-fable-5.5", "claude-fable-5-1"]
    );
    assert_eq!(
        rejected,
        vec![
            LaunchRungRejection {
                model: "claude-fable-5-5".to_string(),
                variant: Some("high".to_string()),
            },
            LaunchRungRejection {
                model: "claude-fable-5-5".to_string(),
                variant: None,
            },
        ]
    );
}

#[test]
fn an_unreleased_gate_drops_every_5_5_rung_without_recording_a_rejection() {
    let mut rejected = Vec::new();

    let selected = select_launch_rungs(
        vec![
            rung("claude-fable-5-5", Some("max")),
            rung("claude-fable-5-1", Some("max")),
        ],
        LaunchGate {
            released: false,
            accepted: &["max"],
        },
        |rejection| rejected.push(rejection),
    );

    assert_eq!(
        selected
            .iter()
            .map(|r| r.model.as_str())
            .collect::<Vec<_>>(),
        vec!["claude-fable-5-1"]
    );
    assert_eq!(rejected, Vec::<LaunchRungRejection>::new());
}

#[test]
fn given_fable_5_5_is_not_released_then_the_latest_model_table_does_not_name_it_yet() {
    let claude_fable = LATEST_MODEL_BY_FAMILY
        .iter()
        .find(|(family, _)| *family == "claudeFable")
        .map(|(_, model)| *model);
    assert!(FABLE_55_RELEASED || claude_fable != Some(FABLE_55_LAUNCH_ID));
    if !FABLE_55_RELEASED {
        assert_eq!(Some(launch_default_model_id()), claude_fable);
    }
}
