use maho_omo_model_profile::builtin_profiles::builtin_model_profiles;
use maho_omo_model_profile::describe::{
    ModelProfileRungSource, ModelProfileRungState, ModelProfileSelection, describe_model_profile,
};
use serde_json::json;

const KIMI: &str = "moonshotai/kimi-k3";
const GLM: &str = "zai/glm-5.3";

fn available(selectors: &[&str]) -> Vec<String> {
    selectors.iter().map(|selector| (*selector).to_owned()).collect()
}

/// The label `daily-normal`'s first rung renders as, read from the builtin table so the case never
/// restates a model id.
fn claude_rung_label() -> String {
    let rung = &builtin_model_profiles()["daily-normal"].models[0];
    format!("{}/{}", rung.providers[0], rung.model)
}

fn kimi_rung_label() -> String {
    let rung = &builtin_model_profiles()["daily-normal"].models[1];
    format!("{}/{}", rung.providers[0], rung.model)
}

#[test]
fn the_unset_walk_picks_the_first_resolving_lane_and_reports_its_rung_states() {
    let view = describe_model_profile(&json!({}), &available(&[KIMI, GLM]));
    assert_eq!(view.profile, "daily-normal");
    assert_eq!(view.selection, ModelProfileSelection::Default);
    assert_eq!(view.applied.as_deref(), Some(KIMI));
    assert_eq!(
        view.rungs.iter().map(|rung| rung.state).collect::<Vec<_>>(),
        [
            ModelProfileRungState::NoLogin,
            ModelProfileRungState::Applied,
            ModelProfileRungState::NotReached,
        ]
    );
    assert_eq!(view.rungs[0].model, claude_rung_label());
    assert_eq!(view.rungs[1].model, KIMI);
    assert_eq!(view.rungs[2].model, GLM);
    assert!(view.rungs.iter().all(|rung| rung.source == ModelProfileRungSource::BuiltIn));
    assert_eq!(view.reason, None);
}

#[test]
fn an_exclude_entry_marks_its_rung_excluded_and_removes_the_selector_before_resolving() {
    let view = describe_model_profile(&json!({"exclude_models": ["kimi-*"]}), &available(&[KIMI, GLM]));
    assert_eq!(view.applied.as_deref(), Some(GLM));
    let excluded = view.rungs.iter().find(|rung| rung.model == kimi_rung_label()).expect("the kimi rung");
    assert_eq!(excluded.state, ModelProfileRungState::Excluded);
    assert_eq!(excluded.excluded_by.as_deref(), Some("kimi-*"));
    assert_eq!(view.rungs[2].state, ModelProfileRungState::Applied);
}

#[test]
fn a_configured_profile_is_a_set_selection_and_a_user_chain_reports_omo_json_rungs() {
    let view = describe_model_profile(
        &json!({"model_profile": "mine", "model_profiles": {"mine": {"models": [KIMI]}}}),
        &available(&[KIMI]),
    );
    assert_eq!(view.profile, "mine");
    assert_eq!(view.selection, ModelProfileSelection::Set);
    assert_eq!(view.applied.as_deref(), Some(KIMI));
    assert_eq!(view.rungs.len(), 1);
    assert_eq!(view.rungs[0].source, ModelProfileRungSource::OmoJson);
    assert_eq!(view.rungs[0].state, ModelProfileRungState::Applied);
}

#[test]
fn a_literal_pin_is_one_pin_rung_of_itself() {
    let view = describe_model_profile(&json!({"model_profile": KIMI}), &available(&[KIMI]));
    assert_eq!(view.profile, KIMI);
    assert_eq!(view.selection, ModelProfileSelection::Set);
    assert_eq!(view.applied.as_deref(), Some(KIMI));
    assert_eq!(view.rungs.len(), 1);
    assert_eq!(view.rungs[0].source, ModelProfileRungSource::Pin);
    assert_eq!(view.rungs[0].state, ModelProfileRungState::Applied);
}

#[test]
fn a_retired_id_counts_as_unset_unless_the_user_defines_that_name() {
    let retired = describe_model_profile(&json!({"model_profile": "recommended"}), &available(&[KIMI, GLM]));
    assert_eq!(retired.profile, "daily-normal");
    assert_eq!(retired.selection, ModelProfileSelection::Default);

    let user_defined = describe_model_profile(
        &json!({"model_profile": "recommended", "model_profiles": {"recommended": {"models": [KIMI]}}}),
        &available(&[KIMI]),
    );
    assert_eq!(user_defined.profile, "recommended");
    assert_eq!(user_defined.selection, ModelProfileSelection::Set);
    assert_eq!(user_defined.applied.as_deref(), Some(KIMI));
}

#[test]
fn an_unknown_name_and_an_empty_profile_carry_their_reason() {
    let unknown = describe_model_profile(&json!({"model_profile": "nope"}), &available(&[KIMI]));
    assert_eq!(unknown.profile, "nope");
    assert!(unknown.rungs.is_empty());
    assert_eq!(
        unknown.reason.as_deref(),
        Some("model_profile \"nope\" is not defined; known profiles: daily-heavy, daily-normal, geeky-heavy, geeky-normal")
    );

    let empty = describe_model_profile(
        &json!({"model_profile": "mine", "model_profiles": {"mine": {"display_name": "Mine"}}}),
        &available(&[KIMI]),
    );
    assert_eq!(empty.profile, "mine");
    assert!(empty.rungs.is_empty());
    assert_eq!(empty.reason.as_deref(), Some("the profile lists no models"));
}

#[test]
fn a_default_lane_that_cannot_resolve_reports_its_rungs_as_no_login() {
    let view = describe_model_profile(&json!({}), &available(&["example/nothing-in-any-chain"]));
    assert_eq!(view.profile, "daily-normal");
    assert_eq!(view.selection, ModelProfileSelection::Default);
    assert_eq!(view.applied, None);
    assert_eq!(
        view.rungs.iter().map(|rung| rung.state).collect::<Vec<_>>(),
        [
            ModelProfileRungState::NoLogin,
            ModelProfileRungState::NoLogin,
            ModelProfileRungState::NoLogin,
        ]
    );
}
