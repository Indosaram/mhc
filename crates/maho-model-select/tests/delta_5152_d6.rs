//! Tests for delta D6 of spec model-selection: the Kimi rung of `daily-normal` and the
//! non-empty-providers invariant of built-in rungs.

use maho_model_select::*;

fn list(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn daily_normal(profiles: &[ModelProfile]) -> &ModelProfile {
    profiles
        .iter()
        .find(|p| p.id == "daily-normal")
        .expect("daily-normal")
}

fn kimi_ranking() -> Vec<String> {
    list(maho_ext_recommended_models::ranked_providers(
        "kimi-k3.1-pro",
    ))
}

// 1. The Kimi rung names kimi-k3.1-pro with the recommended ranking and level.
#[test]
fn daily_normal_kimi_rung_follows_recommended_data() {
    let profiles = builtin_profiles();
    let profile = daily_normal(&profiles);
    let rung = profile
        .rungs
        .iter()
        .find(|r| r.model == "kimi-k3.1-pro")
        .expect("kimi-k3.1-pro rung");
    assert!(!kimi_ranking().is_empty());
    assert_eq!(rung.providers, kimi_ranking());
    assert_eq!(rung.reasoning.as_deref(), Some("max"));
    for released in [false, true] {
        for profile in build_builtin_profiles(released, &mut |_: &str| {}).profiles {
            assert!(profile.rungs.iter().all(|r| r.model != "kimi-k3"));
        }
    }
}

// 2. Every rung of every built-in profile has a non-empty provider list (both gate states).
#[test]
fn every_builtin_rung_has_providers() {
    for released in [false, true] {
        for profile in build_builtin_profiles(released, &mut |_: &str| {}).profiles {
            for rung in &profile.rungs {
                assert!(
                    !rung.providers.is_empty(),
                    "{}: rung {} has no providers",
                    profile.id,
                    rung.model
                );
                assert_eq!(
                    rung.providers,
                    list(maho_ext_recommended_models::ranked_providers(&rung.model)),
                    "{}: rung {} providers differ from the ranking",
                    profile.id,
                    rung.model
                );
                let data_level = maho_ext_recommended_models::RECOMMENDED_DEFAULT_MODELS
                    .iter()
                    .any(|(id, _)| *id == rung.model);
                if data_level {
                    assert!(
                        rung.reasoning.is_some(),
                        "{}: rung {} has no level",
                        profile.id,
                        rung.model
                    );
                }
            }
        }
    }
}

// 3. A gateway outside the Kimi ranking never serves the Kimi rung.
#[test]
fn kimi_rung_does_not_match_through_unlisted_gateway() {
    let profiles = merge_profiles(None);
    let r = resolve_profile(
        &profiles,
        "daily-normal",
        &list(&["acme-gateway/kimi-k3.1-pro"]),
        &Exclusions::default(),
    );
    assert_eq!(
        r,
        ProfileResolution::Unavailable {
            profile: "daily-normal".into(),
            chain: list(&[
                "anthropic-subscription/claude-opus-5-5",
                "kimi-coding/kimi-k3.1-pro",
                "zai-coding-plan/glm-5.3",
            ]),
        }
    );
}

// 4. Kimi Code's bare id resolves the Kimi rung.
#[test]
fn kimi_rung_matches_bare_kimi_coding_id() {
    let profiles = merge_profiles(None);
    let r = resolve_profile(
        &profiles,
        "daily-normal",
        &list(&["kimi-coding/k3.1-pro"]),
        &Exclusions::default(),
    );
    assert_eq!(
        r,
        ProfileResolution::Resolved {
            profile: "daily-normal".into(),
            provider: "kimi-coding".into(),
            model_id: "k3.1-pro".into(),
            reasoning: Some("max".into()),
            skipped: list(&["anthropic-subscription/claude-opus-5-5"]),
            pinned: false,
        }
    );
}

// 5. The bare-id transform applies to the Kimi Code providers only.
#[test]
fn kimi_k31_pro_transform() {
    assert_eq!(
        model_id_for_provider("kimi-for-coding", "kimi-k3.1-pro"),
        "k3.1-pro"
    );
    assert_eq!(
        model_id_for_provider("kimi-coding", "kimi-k3.1-pro-highspeed"),
        "k3.1-pro-highspeed"
    );
    assert_eq!(
        model_id_for_provider("moonshotai", "kimi-k3.1-pro"),
        "kimi-k3.1-pro"
    );
}
