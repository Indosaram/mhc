use maho_ai::types::ModelThinkingLevel;
use maho_ext_api::{ExtensionMode, SessionReason};
use maho_model_select::ext::{AppliedSessions, should_apply, thinking_level_for};
use maho_model_select::probe::{
    Account, AuthProbe, ProbeError, ProbeReason, probe_and_resolve, rotation_allowed_by_models_json,
};
use maho_model_select::*;
use serde_json::json;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;

fn list(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

// ---- Grammar ---------------------------------------------------------------------------

#[test]
fn grammar_cases() {
    let parsed = parse_model_string("openai/gpt-x:max").expect("parsed");
    assert_eq!(
        (
            parsed.provider.as_str(),
            parsed.model_id.as_str(),
            parsed.variant.as_deref()
        ),
        ("openai", "gpt-x", Some("max"))
    );
    assert_eq!(
        split_reasoning_suffix("gpt-x:max", false),
        ("gpt-x:max".to_string(), None)
    );
    assert_eq!(
        split_reasoning_suffix("a/gpt-x:MAX", false),
        ("a/gpt-x".to_string(), Some("max".to_string()))
    );
    assert_eq!(
        split_reasoning_suffix(":high", false),
        (":high".to_string(), None)
    );
    assert_eq!(
        parse_variant("claude-opus (thinking)"),
        ("claude-opus".to_string(), Some("thinking".to_string()))
    );
    assert_eq!(
        parse_variant("a/b HIGH"),
        ("a/b".to_string(), Some("high".to_string()))
    );
    assert_eq!(normalize_reasoning(" None "), Some("off".to_string()));
    assert_eq!(normalize_reasoning("turbo"), Some("turbo".to_string()));
    assert_eq!(normalize_reasoning("  "), None);
    assert_eq!(clamp_reasoning("xhigh", &["low", "high"]), Some("high"));
    assert_eq!(clamp_reasoning("minimal", &["high"]), None);
    assert_eq!(clamp_reasoning("turbo", &["high"]), None);
    assert_eq!(normalize_model(Some("  ")), None);
    assert_eq!(normalize_model_id("gpt-5.4"), "gpt-5-4");
    assert_eq!(normalize_model_id("v.x"), "v.x");
    assert!(parse_model_string("/x").is_none());
}

// ---- Matching --------------------------------------------------------------------------

#[test]
fn matching_cases() {
    assert_eq!(
        match_available(
            "kimi-k2.7",
            &list(&["acme/kimi-k2-7"]),
            Some(&list(&["acme"]))
        )
        .as_deref(),
        Some("acme/kimi-k2-7")
    );
    assert_eq!(
        match_available("gpt-7-2", &list(&["acme/gpt-7.2"]), None).as_deref(),
        Some("acme/gpt-7.2")
    );
    let both = list(&["sub/claude-sonnet-9", "gw/claude-sonnet-9"]);
    assert_eq!(
        match_available("claude-sonnet-9", &both, None).as_deref(),
        Some("sub/claude-sonnet-9")
    );
    assert_eq!(
        match_available("claude-sonnet-9", &both, Some(&list(&["gw", "sub"]))).as_deref(),
        Some("gw/claude-sonnet-9")
    );
    assert_eq!(
        match_available(
            "claude-haiku",
            &list(&["gw/claude-haiku-9-2030", "sub/claude-haiku-9"]),
            None
        )
        .as_deref(),
        Some("sub/claude-haiku-9")
    );
    let widgets = list(&["acme/widget-mini", "widget-with-a-long-suffix"]);
    assert_eq!(
        match_available("widget", &widgets, None).as_deref(),
        Some("widget-with-a-long-suffix")
    );
    let reversed: Vec<String> = widgets.into_iter().rev().collect();
    assert_eq!(
        match_available("widget", &reversed, None).as_deref(),
        Some("widget-with-a-long-suffix")
    );
    assert!(is_available("gpt-7.2", &list(&["acme/gpt-7-2"])));
    assert!(!is_available("nothing", &list(&["acme/gpt-7-2"])));
}

// ---- Transforms ------------------------------------------------------------------------

#[test]
fn transform_cases() {
    assert_eq!(model_id_for_provider("vercel", "gpt-7"), "openai/gpt-7");
    assert_eq!(model_id_for_provider("vercel", "grok-9"), "xai/grok-9");
    assert_eq!(model_id_for_provider("vercel", "custom/abc"), "custom/abc");
    assert_eq!(model_id_for_provider("vercel", "mystery-1"), "mystery-1");
    assert_eq!(
        model_id_for_provider("vercel", "claude-sonnet-8-1"),
        "anthropic/claude-sonnet-8.1"
    );
    assert_eq!(
        model_id_for_provider("github-copilot", "claude-sonnet-8-1"),
        "claude-sonnet-8.1"
    );
    assert_eq!(
        model_id_for_provider("google", "gemini-3-flash"),
        "gemini-3-flash-preview"
    );
    assert_eq!(
        model_id_for_provider("google", "antigravity-gemini-3-flash"),
        "antigravity-gemini-3-flash"
    );
    assert_eq!(
        model_id_for_provider("google", "gemini-3-flash-lite"),
        "gemini-3-flash-lite"
    );
    assert_eq!(
        model_id_for_provider("google", "gemini-3.1-pro"),
        "gemini-3.1-pro-preview"
    );
    assert_eq!(
        model_id_for_provider("kimi-for-coding", "kimi-k3-256k"),
        "k3-256k"
    );
    assert_eq!(model_id_for_provider("acme", "kimi-k3"), "kimi-k3");
}

// ---- Delegated choice ------------------------------------------------------------------

fn input() -> DelegateInput {
    DelegateInput::default()
}

fn resolved(choice: DelegateChoice) -> Resolved {
    match choice {
        DelegateChoice::Resolved(r) => r,
        other => panic!("expected a model, got {other:?}"),
    }
}

#[test]
fn user_fallback_is_used_when_primary_is_missing() {
    let r = resolved(resolve_delegate_model(&DelegateInput {
        user_model: Some("a/x".into()),
        user_fallbacks: list(&["b/y"]),
        available: list(&["b/y"]),
        ..input()
    }));
    assert_eq!(r.model, "b/y");
    assert!(r.from_fallback);
}

#[test]
fn user_model_with_variant_is_not_validated_on_cold_cache() {
    let r = resolved(resolve_delegate_model(&DelegateInput {
        user_model: Some("a/x:high".into()),
        ..input()
    }));
    assert_eq!(
        (r.model.as_str(), r.variant.as_deref(), r.from_fallback),
        ("a/x", Some("high"), false)
    );
}

#[test]
fn nothing_known_defers() {
    assert_eq!(resolve_delegate_model(&input()), DelegateChoice::Deferred);
}

#[test]
fn explicit_high_default_is_kept() {
    let r = resolved(resolve_delegate_model(&DelegateInput {
        category_default: Some("acme/gpt-x-high".into()),
        available: list(&["acme/gpt-x-high-preview"]),
        ..input()
    }));
    assert_eq!(r.model, "acme/gpt-x-high");
    let r = resolved(resolve_delegate_model(&DelegateInput {
        category_default: Some("acme/gpt-x".into()),
        available: list(&["acme/gpt-x-preview"]),
        ..input()
    }));
    assert_eq!(r.model, "acme/gpt-x-preview");
}

#[test]
fn cold_chain_uses_a_connected_provider() {
    let r = resolved(resolve_delegate_model(&DelegateInput {
        connected_providers: Some(list(&["kimi-for-coding"])),
        chain: vec![
            ChainRung {
                providers: list(&["acme"]),
                model: "m1".into(),
                variant: None,
            },
            ChainRung {
                providers: list(&["kimi-for-coding"]),
                model: "kimi-for-coding/kimi-k3-256k".into(),
                variant: Some("low".into()),
            },
        ],
        ..input()
    }));
    assert_eq!(r.model, "kimi-for-coding/k3-256k");
    assert_eq!(r.variant.as_deref(), Some("low"));
    assert_eq!(r.rung, Some(1));
    assert!(r.from_fallback);
}

#[test]
fn warm_chain_matches_transformed_ids() {
    let r = resolved(resolve_delegate_model(&DelegateInput {
        chain: vec![ChainRung {
            providers: list(&["vercel"]),
            model: "claude-sonnet-8-1".into(),
            variant: None,
        }],
        available: list(&["vercel/anthropic/claude-sonnet-8.1"]),
        ..input()
    }));
    assert_eq!(r.model, "vercel/anthropic/claude-sonnet-8.1");
    assert_eq!(r.rung, Some(0));
}

#[test]
fn system_default_is_the_last_resort() {
    let r = resolved(resolve_delegate_model(&DelegateInput {
        system_default: Some(" p/m ".into()),
        available: list(&["z/unrelated"]),
        chain: vec![ChainRung {
            providers: list(&["acme"]),
            model: "m1".into(),
            variant: None,
        }],
        ..input()
    }));
    assert_eq!(r.model, "p/m");
    assert_eq!(
        resolve_delegate_model(&DelegateInput {
            available: list(&["z/unrelated"]),
            ..input()
        }),
        DelegateChoice::None
    );
}

// ---- Model profiles --------------------------------------------------------------------

fn resolved_model(result: &ProfileResolution) -> Option<(String, String, String)> {
    match result {
        ProfileResolution::Resolved {
            profile,
            provider,
            model_id,
            ..
        } => Some((profile.clone(), provider.clone(), model_id.clone())),
        _ => None,
    }
}

#[test]
fn builtin_ids_exist_in_order() {
    let ids: Vec<String> = builtin_profiles().into_iter().map(|p| p.id).collect();
    assert_eq!(
        ids,
        list(&["daily-normal", "daily-heavy", "geeky-normal", "geeky-heavy"])
    );
    for profile in builtin_profiles() {
        assert!(!profile.rungs.is_empty());
        assert_eq!(profile.family.as_deref(), profile.id.split('-').next());
    }
}

#[test]
fn default_lanes_pick_the_first_resolving_profile() {
    let profiles = merge_profiles(None);
    let top = &profiles[0].rungs[0];
    let available = vec![format!("anthropic-subscription/{}", top.model)];
    let r = resolve_default(&profiles, &available, &Exclusions::default());
    assert_eq!(
        resolved_model(&r).map(|m| m.0),
        Some("daily-normal".to_string())
    );

    let gpt = &profiles[2].rungs[0];
    let available = vec![format!("chatgpt-subscription/{}", gpt.model)];
    let r = resolve_default(&profiles, &available, &Exclusions::default());
    assert_eq!(
        resolved_model(&r).map(|m| m.0),
        Some("geeky-normal".to_string())
    );

    let r = resolve_default(&profiles, &[], &Exclusions::default());
    assert!(
        matches!(r, ProfileResolution::Unavailable { ref profile, .. } if profile == "daily-normal")
    );
}

#[test]
fn pins_resolve_with_their_reasoning() {
    let profiles = merge_profiles(None);
    let r = resolve_profile(
        &profiles,
        "p/m:high",
        &list(&["p/m"]),
        &Exclusions::default(),
    );
    match r {
        ProfileResolution::Resolved {
            provider,
            model_id,
            reasoning,
            pinned,
            ..
        } => {
            assert_eq!(
                (
                    provider.as_str(),
                    model_id.as_str(),
                    reasoning.as_deref(),
                    pinned
                ),
                ("p", "m", Some("high"), true)
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn user_entry_replaces_the_builtin_chain() {
    let profiles = merge_profiles(Some(&json!({"daily-normal": {"display_name": "Mine"}})));
    assert_eq!(profiles[0].display_name, "Mine");
    let r = resolve_profile(
        &profiles,
        "daily-normal",
        &list(&["anthropic-subscription/claude-opus-5-5"]),
        &Exclusions::default(),
    );
    assert_eq!(
        r,
        ProfileResolution::Empty {
            profile: "daily-normal".into()
        }
    );
}

#[test]
fn user_rungs_with_providers_match_exactly() {
    let profiles = merge_profiles(Some(
        &json!({"mine": {"models": ["p/m"]}, "bare": {"models": ["m"]}}),
    ));
    let available = list(&["p/m-2024"]);
    assert!(matches!(
        resolve_profile(&profiles, "mine", &available, &Exclusions::default()),
        ProfileResolution::Unavailable { .. }
    ));
    let r = resolve_profile(&profiles, "bare", &available, &Exclusions::default());
    assert_eq!(
        resolved_model(&r).map(|m| (m.1, m.2)),
        Some(("p".to_string(), "m-2024".to_string()))
    );
}

#[test]
fn unknown_profile_lists_sorted_ids() {
    let profiles = merge_profiles(Some(&json!({"aaa": {}})));
    match resolve_profile(&profiles, "nope", &[], &Exclusions::default()) {
        ProfileResolution::Unknown { known, message, .. } => {
            let mut sorted = known.clone();
            sorted.sort();
            assert_eq!(known, sorted);
            assert!(known.contains(&"aaa".to_string()));
            assert!(message.contains("nope"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn retired_id_is_unset_unless_defined() {
    assert_eq!(effective_profile(Some("recommended"), None), None);
    assert_eq!(
        effective_profile(Some("recommended"), Some(&json!({"recommended": {}}))).as_deref(),
        Some("recommended")
    );
    assert_eq!(
        effective_profile(Some(" daily-heavy "), None).as_deref(),
        Some("daily-heavy")
    );
}

// ---- Probe -------------------------------------------------------------------------------

#[derive(Default)]
struct FakeProbe {
    no_rotation: Vec<String>,
    accounts: HashMap<String, Vec<Account>>,
    provider_errors: HashMap<String, ProbeError>,
    request_errors: Vec<String>,
    slots_tried: Mutex<Vec<(String, Option<String>)>>,
}

impl AuthProbe for FakeProbe {
    fn rotation_allowed(&self, provider: &str) -> impl Future<Output = bool> + Send {
        let allowed = !self.no_rotation.iter().any(|p| p == provider);
        async move { allowed }
    }
    fn accounts(&self, provider: &str) -> impl Future<Output = Vec<Account>> + Send {
        let accounts = self.accounts.get(provider).cloned().unwrap_or_default();
        async move { accounts }
    }
    fn provider_auth(
        &self,
        provider: &str,
        slot: Option<&str>,
    ) -> impl Future<Output = Result<(), ProbeError>> + Send {
        self.slots_tried
            .lock()
            .expect("lock")
            .push((provider.to_owned(), slot.map(str::to_owned)));
        let result = self
            .provider_errors
            .get(provider)
            .cloned()
            .map_or(Ok(()), Err);
        async move { result }
    }
    fn request_auth(
        &self,
        provider: &str,
        model: &str,
        _slot: Option<&str>,
    ) -> impl Future<Output = Result<(), ProbeError>> + Send {
        let failing = self.request_errors.contains(&format!("{provider}/{model}"));
        async move {
            if failing {
                Err(ProbeError {
                    class: Some("HeaderError".into()),
                    code: None,
                })
            } else {
                Ok(())
            }
        }
    }
}

fn two_rung_profiles() -> Vec<ModelProfile> {
    merge_profiles(Some(&json!({"lane": {"models": ["a/m1", "b/m2"]}})))
}

#[tokio::test]
async fn refresh_failure_excludes_the_provider() {
    let probe = FakeProbe {
        provider_errors: HashMap::from([(
            "a".to_string(),
            ProbeError {
                class: Some("AuthError".into()),
                code: Some("oauth".into()),
            },
        )]),
        ..FakeProbe::default()
    };
    let profiles = two_rung_profiles();
    let available = list(&["a/m1", "b/m2"]);
    let (result, failures) = probe_and_resolve(&probe, |ex| {
        resolve_profile(&profiles, "lane", &available, ex)
    })
    .await;
    assert_eq!(resolved_model(&result).map(|m| m.1), Some("b".to_string()));
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].reason, ProbeReason::Refresh);
    assert_eq!(failures[0].fingerprint, "AuthError:oauth");
}

#[tokio::test]
async fn request_failure_excludes_only_the_model() {
    let probe = FakeProbe {
        request_errors: list(&["a/m1"]),
        ..FakeProbe::default()
    };
    let profiles = merge_profiles(Some(&json!({"lane": {"models": ["a/m1", "a/m3"]}})));
    let available = list(&["a/m1", "a/m3"]);
    let (result, failures) = probe_and_resolve(&probe, |ex| {
        resolve_profile(&profiles, "lane", &available, ex)
    })
    .await;
    assert_eq!(resolved_model(&result).map(|m| m.2), Some("m3".to_string()));
    assert_eq!(failures[0].reason, ProbeReason::Request);
}

#[tokio::test]
async fn disabled_rotation_probes_only_the_default_credential() {
    let probe = FakeProbe {
        no_rotation: list(&["a"]),
        accounts: HashMap::from([(
            "a".to_string(),
            vec![
                Account {
                    name: "one".into(),
                    pinned: false,
                },
                Account {
                    name: "two".into(),
                    pinned: false,
                },
            ],
        )]),
        ..FakeProbe::default()
    };
    let profiles = two_rung_profiles();
    let available = list(&["a/m1"]);
    let _ = probe_and_resolve(&probe, |ex| {
        resolve_profile(&profiles, "lane", &available, ex)
    })
    .await;
    assert_eq!(
        probe.slots_tried.lock().expect("lock").clone(),
        vec![("a".to_string(), None)]
    );
}

#[tokio::test]
async fn pins_are_never_probed() {
    let probe = FakeProbe {
        provider_errors: HashMap::from([("p".to_string(), ProbeError::default())]),
        ..FakeProbe::default()
    };
    let profiles = merge_profiles(None);
    let available = list(&["p/m"]);
    let (result, failures) = probe_and_resolve(&probe, |ex| {
        resolve_profile(&profiles, "p/m", &available, ex)
    })
    .await;
    assert!(resolved_model(&result).is_some());
    assert!(failures.is_empty());
    assert!(probe.slots_tried.lock().expect("lock").is_empty());
}

#[test]
fn models_json_can_disable_rotation() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(rotation_allowed_by_models_json(dir.path(), "a"));
    std::fs::write(
        dir.path().join("models.json"),
        r#"{ // c
            "providers": {"a": {"credentials": {"rotation": false}}}}"#,
    )
    .expect("write");
    assert!(!rotation_allowed_by_models_json(dir.path(), "a"));
    assert!(rotation_allowed_by_models_json(dir.path(), "b"));
}

// ---- When to apply -----------------------------------------------------------------------

#[test]
fn apply_conditions() {
    assert!(should_apply(
        SessionReason::Startup,
        Some("provider-default"),
        ExtensionMode::Rpc
    ));
    assert!(should_apply(
        SessionReason::New,
        Some("first-available"),
        ExtensionMode::Print
    ));
    assert!(!should_apply(
        SessionReason::Resume,
        Some("provider-default"),
        ExtensionMode::Rpc
    ));
    assert!(!should_apply(
        SessionReason::Startup,
        Some("cli"),
        ExtensionMode::Rpc
    ));
    assert!(!should_apply(
        SessionReason::Startup,
        Some("scoped"),
        ExtensionMode::Rpc
    ));
    assert!(!should_apply(
        SessionReason::Startup,
        None,
        ExtensionMode::Rpc
    ));
    assert!(!should_apply(
        SessionReason::Startup,
        Some("provider-default"),
        ExtensionMode::Tui
    ));
    let applied = AppliedSessions::default();
    assert!(applied.first("s1"));
    assert!(!applied.first("s1"));
    assert!(applied.first("s2"));
}

#[test]
fn thinking_levels_follow_the_rung() {
    assert_eq!(
        thinking_level_for(Some("none")),
        Some(ModelThinkingLevel::Off)
    );
    assert_eq!(
        thinking_level_for(Some("high")),
        Some(ModelThinkingLevel::High)
    );
    assert_eq!(thinking_level_for(Some("auto")), None);
    assert_eq!(thinking_level_for(Some("turbo")), None);
    assert_eq!(thinking_level_for(None), None);
}
