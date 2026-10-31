//! Tests for delta D7 of spec model-selection: launch-gated ids in the delegated-task choice
//! (flag off; providers invented except the gated id).

use maho_model_select::*;

fn list(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn rung(model: &str) -> ChainRung {
    ChainRung {
        providers: list(&["acme"]),
        model: model.to_owned(),
        variant: None,
    }
}

fn resolved(choice: DelegateChoice) -> Resolved {
    match choice {
        DelegateChoice::Resolved(r) => r,
        other => panic!("expected a model, got {other:?}"),
    }
}

fn cold_connected() -> DelegateInput {
    DelegateInput {
        connected_providers: Some(list(&["acme"])),
        ..DelegateInput::default()
    }
}

// D7 test 1: a built-in gated category default is skipped; the chain is used.
#[test]
fn gated_builtin_category_default_is_skipped_on_cold_cache() {
    let input = DelegateInput {
        category_default: Some("acme/claude-fable-5-5".into()),
        chain: vec![rung("claude-fable-5-1")],
        ..cold_connected()
    };
    let r = resolved(resolve_delegate_model_gated(&input, false));
    assert_eq!(r.model, "acme/claude-fable-5-1");
    assert_eq!(r.rung, Some(0));
    assert!(r.from_fallback);
}

// D7 test 2: a user-configured category default is never filtered.
#[test]
fn user_configured_gated_category_default_is_returned_as_written() {
    let input = DelegateInput {
        category_default: Some("acme/claude-fable-5-5".into()),
        category_default_is_user_configured: true,
        chain: vec![rung("claude-fable-5-1")],
        ..cold_connected()
    };
    let r = resolved(resolve_delegate_model_gated(&input, false));
    assert_eq!(r.model, "acme/claude-fable-5-5");
    assert!(!r.from_fallback);
    assert_eq!(r.rung, None);
}

// D7 test 3: a gated chain rung is skipped; the next rung with a connected provider wins.
#[test]
fn gated_chain_rung_is_skipped_on_cold_cache_with_connected_providers() {
    let input = DelegateInput {
        chain: vec![rung("claude-fable-5.5"), rung("m2")],
        ..cold_connected()
    };
    let r = resolved(resolve_delegate_model_gated(&input, false));
    assert_eq!(r.model, "acme/m2");
    assert_eq!(r.rung, Some(1));
    assert!(r.from_fallback);
}

// D7 test 4: a warm cache is unchanged.
#[test]
fn warm_cache_still_matches_the_gated_id() {
    let input = DelegateInput {
        available: list(&["acme/claude-fable-5-5"]),
        chain: vec![rung("claude-fable-5-5")],
        ..DelegateInput::default()
    };
    let r = resolved(resolve_delegate_model_gated(&input, false));
    assert_eq!(r.model, "acme/claude-fable-5-5");
    assert_eq!(r.rung, Some(0));
}

// D7 test 5: no cache information at all still defers.
#[test]
fn cold_cache_without_connected_list_defers() {
    let input = DelegateInput {
        chain: vec![rung("claude-fable-5-5")],
        ..DelegateInput::default()
    };
    assert_eq!(
        resolve_delegate_model_gated(&input, false),
        DelegateChoice::Deferred
    );
}

// D7 "with the flag on, nothing in §5.3 changes"; the shipped flag is off.
#[test]
fn released_flag_keeps_the_gated_rung_and_shipped_flag_gates() {
    let input = DelegateInput {
        chain: vec![rung("claude-fable-5.5"), rung("m2")],
        ..cold_connected()
    };
    let r = resolved(resolve_delegate_model_gated(&input, true));
    assert_eq!(r.rung, Some(0));
    assert!(!maho_ext_recommended_models::FABLE55_RELEASED);
    assert_eq!(
        resolve_delegate_model(&input),
        resolve_delegate_model_gated(&input, false)
    );
}
